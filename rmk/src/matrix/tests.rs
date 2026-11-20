use core::convert::Infallible;
use core::future::{Future, poll_fn};
use core::task::{Context, Poll, Waker};
use std::cell::RefCell;
use std::rc::Rc;

use embassy_time::{Duration, Instant, MockDriver};
use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use embedded_hal_async::digital::Wait;

use super::Matrix;
use crate::debounce::default_debouncer::DefaultDebouncer;
use crate::event::KeyboardEvent;
use crate::input_device::InputDevice;

#[derive(Default)]
struct Pins {
    pressed: bool,
    selected: bool,
    reads: usize,
    waiter: Option<Waker>,
    immediate_error: bool,
}

struct Input(Rc<RefCell<Pins>>);
struct Output(Rc<RefCell<Pins>>);

impl ErrorType for Input {
    type Error = embedded_hal::digital::ErrorKind;
}

impl InputPin for Input {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        let mut pins = self.0.borrow_mut();
        pins.reads += 1;
        if pins.immediate_error {
            assert!(
                pins.reads < 32,
                "read_event did not yield while GPIO operations kept failing"
            );
            return Err(embedded_hal::digital::ErrorKind::Other);
        }
        Ok(pins.pressed && pins.selected)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.is_high().map(|high| !high)
    }
}

impl Wait for Input {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        poll_fn(|cx| {
            let mut pins = self.0.borrow_mut();
            if pins.immediate_error {
                return Poll::Ready(Err(embedded_hal::digital::ErrorKind::Other));
            }
            if pins.pressed && pins.selected {
                Poll::Ready(Ok(()))
            } else {
                pins.waiter = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        unreachable!("matrix only waits for high")
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        unreachable!("matrix only waits for high")
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        unreachable!("matrix only waits for high")
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        unreachable!("matrix only waits for high")
    }
}

impl ErrorType for Output {
    type Error = Infallible;
}

impl OutputPin for Output {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().selected = false;
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().selected = true;
        Ok(())
    }
}

type TestMatrix = Matrix<Input, Output, DefaultDebouncer<1, 1>, 1, 1, true>;

fn press(pins: &Rc<RefCell<Pins>>, pressed: bool) {
    let mut pins = pins.borrow_mut();
    pins.pressed = pressed;
    if let Some(waker) = pins.waiter.take() {
        waker.wake();
    }
}

fn event(matrix: &mut TestMatrix, cx: &mut Context<'_>) -> KeyboardEvent {
    let mut read = std::pin::pin!(matrix.read_event());
    for _ in 0..1000 {
        if let Poll::Ready(event) = read.as_mut().poll(cx) {
            return event;
        }
        MockDriver::get().advance(Duration::from_micros(100));
    }
    panic!("matrix did not finish debouncing within 100 ms");
}

#[test]
fn held_keys_keep_scanning_without_a_busy_rescan_loop() {
    let clock = MockDriver::get();
    clock.reset();
    clock.advance(Duration::from_millis(1));
    let pins = Rc::new(RefCell::new(Pins::default()));
    let mut matrix = TestMatrix::new([Input(pins.clone())], [Output(pins.clone())], DefaultDebouncer::new());
    let mut cx = Context::from_waker(std::task::Waker::noop());
    press(&pins, true);
    assert!(event(&mut matrix, &mut cx).pressed);

    let before = pins.borrow().reads;
    let mut read = std::pin::pin!(matrix.read_event());
    for _ in 0..80 {
        assert!(read.as_mut().poll(&mut cx).is_pending());
        clock.advance(Duration::from_micros(100));
    }
    let reads = pins.borrow().reads - before;
    assert!(
        (6..=10).contains(&reads),
        "expected paced release checks over 8 ms, got {reads}"
    );
}

#[test]
fn immediately_failing_gpio_does_not_starve_other_tasks() {
    MockDriver::get().reset();
    let pins = Rc::new(RefCell::new(Pins {
        immediate_error: true,
        ..Default::default()
    }));
    let mut matrix = TestMatrix::new([Input(pins.clone())], [Output(pins.clone())], DefaultDebouncer::new());
    let mut read = std::pin::pin!(matrix.read_event());
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(read.as_mut().poll(&mut cx).is_pending());
}

#[test]
fn pending_debounce_finishes_and_idle_wait_wakes_for_the_next_press() {
    let clock = MockDriver::get();
    clock.reset();
    clock.advance(Duration::from_millis(1));
    let pins = Rc::new(RefCell::new(Pins::default()));
    let mut matrix = TestMatrix::new([Input(pins.clone())], [Output(pins.clone())], DefaultDebouncer::new());
    let mut cx = Context::from_waker(std::task::Waker::noop());
    press(&pins, true);
    let started = Instant::now();
    assert!(event(&mut matrix, &mut cx).pressed);
    assert!(started.elapsed().as_millis() >= u64::from(crate::DEBOUNCE_THRESHOLD));
    press(&pins, false);
    assert!(!event(&mut matrix, &mut cx).pressed);

    {
        let mut read = std::pin::pin!(matrix.read_event());
        for _ in 0..30 {
            assert!(read.as_mut().poll(&mut cx).is_pending());
            clock.advance(Duration::from_micros(100));
        }
        let reads = pins.borrow().reads;
        for _ in 0..50 {
            assert!(read.as_mut().poll(&mut cx).is_pending());
            clock.advance(Duration::from_micros(100));
        }
        assert_eq!(
            pins.borrow().reads,
            reads,
            "idle scanner must block on GPIO, not keep polling"
        );
        press(&pins, true);
        for _ in 0..1000 {
            if let Poll::Ready(event) = read.as_mut().poll(&mut cx) {
                assert!(event.pressed);
                return;
            }
            clock.advance(Duration::from_micros(100));
        }
    }
    panic!("GPIO wake did not restart debouncing");
}
