use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use rmk_config::DcdcReg0Voltage;
use rmk_config::resolved::Hardware;
use rmk_config::resolved::hardware::{BoardConfig, ChipSeries, CommunicationConfig};
use syn::ItemMod;

use crate::codegen::override_helper::{Overwritten, find_overwritten};

/// Expand chip initialization code
///
/// If `peripheral_id` is `None`, it means that the chip initialization is for the central.
/// Otherwise, the `peripheral_id` is the index of the peripheral.
pub(crate) fn expand_chip_init(
    hardware: &Hardware,
    peripheral_id: Option<usize>,
    item_mod: &ItemMod,
) -> TokenStream2 {
    let mut chip_config = None;
    let mut mpsl_lfclk_config = None;
    let mut chip_init = None;
    if let Some((_, items)) = &item_mod.content {
        for item in items {
            if let syn::Item::Fn(item_fn) = item
                && let Some(Ok(overwritten)) = find_overwritten(item_fn)
            {
                match overwritten {
                    Overwritten::ChipConfig => chip_config = Some(item_fn.block.to_token_stream()),
                    Overwritten::MpslLfclkConfig => {
                        if !matches!(hardware.chip.series, ChipSeries::Nrf52)
                            || !hardware.communication.ble_enabled()
                        {
                            return syn::Error::new_spanned(
                                item_fn,
                                "mpsl_lfclk_config requires an nRF52 BLE keyboard",
                            )
                            .to_compile_error();
                        }
                        mpsl_lfclk_config = Some(item_fn.block.to_token_stream());
                    }
                    Overwritten::ChipInit => {
                        let stmts = &item_fn.block.stmts;
                        chip_init = Some(quote! { #(#stmts)* });
                    }
                    _ => (),
                }
            }
        }
    }
    if let Some(chip_init) = chip_init {
        chip_init
    } else if chip_config.is_none() && mpsl_lfclk_config.is_none() {
        chip_init_default(hardware, peripheral_id)
    } else {
        chip_init_with_config(hardware, peripheral_id, chip_config, mpsl_lfclk_config)
    }
}

// Default implementations of chip initialization
pub(crate) fn chip_init_default(hardware: &Hardware, peripheral_id: Option<usize>) -> TokenStream2 {
    chip_init_with_config(hardware, peripheral_id, None, None)
}

fn chip_init_with_config(
    hardware: &Hardware,
    peripheral_id: Option<usize>,
    chip_config: Option<TokenStream2>,
    mpsl_lfclk_config: Option<TokenStream2>,
) -> TokenStream2 {
    let chip = &hardware.chip;
    let communication = &hardware.communication;
    let peri_num = hardware.board.get_num_peripheral();
    match chip.series {
        ChipSeries::Stm32 => {
            let config =
                chip_config.unwrap_or_else(|| quote! { ::embassy_stm32::Config::default() });
            quote! {
                let config = #config;
                let mut p = ::embassy_stm32::init(config);
            }
        }
        ChipSeries::Nrf52 => {
            let chip_cfg = &hardware.chip_config;
            let dcdc_config = if chip.chip == "nrf52840" {
                let reg0_enabled = chip_cfg.dcdc_reg0.unwrap_or(true);
                let reg1_enabled = chip_cfg.dcdc_reg1.unwrap_or(true);
                let (reg0_voltage, reg0_voltage_str) = match chip_cfg.dcdc_reg0_voltage {
                    Some(DcdcReg0Voltage::V1_8) => {
                        (quote! { ::embassy_nrf::config::Reg0Voltage::_1V8 }, "1V8")
                    }
                    _ => (quote! { ::embassy_nrf::config::Reg0Voltage::_3V3 }, "3V3"),
                };
                quote! {
                    config.dcdc.reg0_voltage = Some(#reg0_voltage);
                    config.dcdc.reg0 = #reg0_enabled;
                    config.dcdc.reg1 = #reg1_enabled;
                    ::defmt::info!("DCDC config: reg0_voltage={}, reg0={}, reg1={}", #reg0_voltage_str, #reg0_enabled, #reg1_enabled);
                }
            } else if chip.chip == "nrf52833" {
                let reg1_enabled = chip_cfg.dcdc_reg1.unwrap_or(true);
                // No DC/DC switch for REG0 on this part, but its output voltage is the same
                // UICR.REGOUT0 setting as on the nRF52840. Left untouched unless configured.
                let reg0_voltage = match chip_cfg.dcdc_reg0_voltage {
                    Some(DcdcReg0Voltage::V1_8) => quote! {
                        config.dcdc.reg0_voltage = Some(::embassy_nrf::config::Reg0Voltage::_1V8);
                    },
                    Some(DcdcReg0Voltage::V3_3) => quote! {
                        config.dcdc.reg0_voltage = Some(::embassy_nrf::config::Reg0Voltage::_3V3);
                    },
                    None => quote! {},
                };
                quote! {
                    config.dcdc.reg1 = #reg1_enabled;
                    #reg0_voltage
                    ::defmt::info!("DCDC config: reg1={}", #reg1_enabled);
                }
            } else {
                quote! {}
            };
            let chip_config = match chip_config {
                Some(config) => quote! { let config = #config; },
                None => quote! {
                    let mut config = ::embassy_nrf::config::Config::default();
                    #dcdc_config
                },
            };
            let mpsl_lfclk_config = mpsl_lfclk_config.unwrap_or_else(|| quote! {
                ::nrf_sdc::mpsl::raw::mpsl_clock_lfclk_cfg_t {
                    source: ::nrf_sdc::mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
                    rc_ctiv: ::nrf_sdc::mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
                    rc_temp_ctiv: ::nrf_sdc::mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
                    // nRF52 LFRC is +/-500 ppm after calibration.
                    accuracy_ppm: 500,
                    skip_wait_lfclk_started: ::nrf_sdc::mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
                }
            });
            let ble_addr = get_ble_addr(hardware, peripheral_id);
            let central_links = if peripheral_id.is_some() { 0 } else { peri_num };
            let peripheral_links = if peripheral_id.is_some() {
                quote! { 1 }
            } else {
                quote! { ::rmk::ble::NUM_HOST_LINKS }
            };
            let ble_init = match &communication {
                CommunicationConfig::Ble(_) | CommunicationConfig::Both(_, _) => quote! {
                    // Initialize nrf-sdc and ble stack
                    let mpsl_p = ::nrf_sdc::mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
                    let lfclk_cfg = #mpsl_lfclk_config;
                    static MPSL: ::static_cell::StaticCell<::nrf_sdc::mpsl::MultiprotocolServiceLayer> = ::static_cell::StaticCell::new();
                    static SESSION_MEM: ::static_cell::StaticCell<::nrf_sdc::mpsl::SessionMem<1>> = ::static_cell::StaticCell::new();
                    let mpsl = MPSL.init(::defmt::unwrap!(::nrf_sdc::mpsl::MultiprotocolServiceLayer::with_timeslots(
                        mpsl_p,
                        Irqs,
                        lfclk_cfg,
                        SESSION_MEM.init(::nrf_sdc::mpsl::SessionMem::new())
                    )));
                    spawner.spawn(mpsl_task(&*mpsl).unwrap());
                    let sdc_p = ::nrf_sdc::Peripherals::new(
                        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25, p.PPI_CH26,
                        p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
                    );
                    let mut rng = ::embassy_nrf::rng::Rng::new(p.RNG, Irqs);
                    let mut sdc_mem = ::nrf_sdc::Mem::<{ ::rmk::ble::calculate_sdc_memory_size(#central_links, #peripheral_links, SDC_PACKET_SIZE, SDC_TX_BUFFERS, SDC_RX_BUFFERS) }>::new();
                    let ble_controller = ::defmt::unwrap!(build_sdc(sdc_p, &mut rng, &*mpsl, &mut sdc_mem));
                    let ble_addr = #ble_addr;
                },
                _ => quote! {},
            };
            quote! {
                use embassy_nrf::interrupt::InterruptExt;
                #chip_config
                let p = ::embassy_nrf::init(config);
                #ble_init
            }
        }
        ChipSeries::Rp2040 => {
            let config =
                chip_config.unwrap_or_else(|| quote! { ::embassy_rp::config::Config::default() });
            let ble_addr = get_ble_addr(hardware, peripheral_id);
            if communication.ble_enabled() {
                quote! {
                    let config = #config;
                    #[allow(unused_mut)]
                    let mut p = ::embassy_rp::init(config);

                    #[cfg(feature = "skip-cyw43-firmware")]
                    let (fw, clm, btfw, nvram) = {
                        static EMPTY: &::cyw43::Aligned<::cyw43::A4, [u8]> = &::cyw43::Aligned([0u8; 0]);
                        (EMPTY, &[] as &[u8], EMPTY, EMPTY)
                    };

                    #[cfg(not(feature = "skip-cyw43-firmware"))]
                    let (fw, clm, btfw, nvram) = {
                        // IMPORTANT
                        //
                        // Download and make sure these files from https://github.com/embassy-rs/embassy/tree/main/cyw43-firmware
                        // are available in `./examples/rp-pico-w`. (should be automatic)
                        //
                        // IMPORTANT
                        let fw = ::cyw43::aligned_bytes!("../cyw43-firmware/43439A0.bin");
                        let clm = ::cyw43::aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
                        let btfw = ::cyw43::aligned_bytes!("../cyw43-firmware/43439A0_btfw.bin");
                        let nvram = ::cyw43::aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");
                        (fw, clm, btfw, nvram)
                    };

                    let pwr = ::embassy_rp::gpio::Output::new(p.PIN_23, ::embassy_rp::gpio::Level::Low);
                    let cs = ::embassy_rp::gpio::Output::new(p.PIN_25, ::embassy_rp::gpio::Level::High);
                    let mut pio = ::embassy_rp::pio::Pio::new(p.PIO0, Irqs);
                    let spi = ::cyw43_pio::PioSpi::new(
                        &mut pio.common,
                        pio.sm0,
                        ::cyw43_pio::DEFAULT_CLOCK_DIVIDER,
                        pio.irq0,
                        cs,
                        p.PIN_24,
                        p.PIN_29,
                        ::embassy_rp::dma::Channel::new(p.DMA_CH0, Irqs),
                        ::embassy_rp::dma::Channel::new(p.DMA_CH2, Irqs),
                    );

                    static STATE: ::static_cell::StaticCell<::cyw43::State> = ::static_cell::StaticCell::new();
                    let state = STATE.init(::cyw43::State::new());
                    let (_net_device, bt_device, mut control, runner) = ::cyw43::new_with_bluetooth(state, pwr, spi, fw, btfw, nvram).await;
                    spawner.spawn(cyw43_task(runner).unwrap());
                    control.init(clm).await;

                    let ble_controller: ::bt_hci::controller::ExternalController<_, 10> = ::bt_hci::controller::ExternalController::new(bt_device);
                    let ble_addr = #ble_addr;
                }
            } else {
                quote! {
                    let config = #config;
                    #[allow(unused_mut)]
                    let mut p = ::embassy_rp::init(config);
                }
            }
        }
        ChipSeries::Esp32 => {
            let config = chip_config.unwrap_or_else(|| {
                quote! {
                    ::esp_hal::Config::default().with_cpu_clock(::esp_hal::clock::CpuClock::max())
                }
            });
            let ble_addr = get_ble_addr(hardware, peripheral_id);
            let battery = match peripheral_id {
                Some(id) => &hardware.peripheral_batteries[id],
                None => &hardware.battery,
            };
            // Battery measurement and the optional ADC entropy source both need ADC1.
            let adc_entropy_init = if battery.adc.is_none() {
                quote! { let _trng_source = ::esp_hal::rng::TrngSource::new(p.RNG, p.ADC1); }
            } else {
                quote! {}
            };
            quote! {
                ::esp_println::logger::init_logger_from_env();
                let p = ::esp_hal::init(#config);
                ::esp_alloc::heap_allocator!(size: 72 * 1024);
                let timg0 = ::esp_hal::timer::timg::TimerGroup::new(p.TIMG0);
                ::esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);
                #adc_entropy_init
                let connector = ::esp_radio::ble::controller::BleConnector::new(p.BT, Default::default()).unwrap();
                let ble_controller: ::bt_hci::controller::ExternalController<_, 64> = ::bt_hci::controller::ExternalController::new(connector);
                let ble_addr = #ble_addr;
            }
        }
    }
}

fn get_ble_addr(hardware: &Hardware, peripheral_id: Option<usize>) -> TokenStream2 {
    if hardware.chip.series == ChipSeries::Nrf52 {
        quote! {
            {
                let ficr = ::embassy_nrf::pac::FICR;
                let high = u64::from(ficr.deviceid(1).read());
                let addr = high << 32 | u64::from(ficr.deviceid(0).read());
                let addr = addr | 0x0000_c000_0000_0000;
                let ble_addr = addr.to_le_bytes()[..6].try_into().expect("Failed to read BLE address from FICR");
                ble_addr
            }
        }
    } else {
        // Check whether the address is set in the keyboard.toml, if not, use the default address
        let addr = match &hardware.board {
            BoardConfig::Split(split) => {
                match peripheral_id {
                    Some(id) => {
                        // Split peripheral
                        // The 4th byte is the peripheral index to make sure that the BLE address for each peripheral is different
                        let default_addr = [0x7e, 0xfe, 0x73, id as u8, 0x66, 0xe3];
                        split
                            .peripheral
                            .get(id)
                            .unwrap_or_else(|| panic!("There's no config for peripheral {}", id))
                            .ble_addr
                            .unwrap_or(default_addr)
                    }
                    None => {
                        // Split central
                        let default_addr = [0x18, 0xe2, 0x21, 0x80, 0xc0, 0xc7];
                        split.central.ble_addr.unwrap_or(default_addr)
                    }
                }
            }
            // TODO: allow user to set the BLE address for uni-body keyboards
            BoardConfig::UniBody(_uni_body) => [0x18, 0xe2, 0x21, 0x80, 0xc0, 0xc7],
        };
        quote! {
            [
                #(#addr),*
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use rmk_config::KeyboardTomlConfig;

    use super::chip_init_default;

    fn board_hardware(example: &str) -> rmk_config::resolved::Hardware {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../examples/use_config/{example}/keyboard.toml"));
        KeyboardTomlConfig::new_from_toml_path(&path)
            .hardware()
            .unwrap()
    }

    fn binding_names(tokens: proc_macro2::TokenStream) -> Vec<String> {
        let block: syn::Block = syn::parse2(quote! {{ #tokens }}).unwrap();
        block
            .stmts
            .iter()
            .filter_map(|stmt| {
                let syn::Stmt::Local(local) = stmt else {
                    return None;
                };
                let pat = match &local.pat {
                    syn::Pat::Type(typed) => &*typed.pat,
                    pat => pat,
                };
                let syn::Pat::Ident(ident) = pat else {
                    return None;
                };
                Some(ident.ident.to_string())
            })
            .collect()
    }

    #[test]
    fn chip_config_override_preserves_nrf_ble_initialization_on_both_split_roles() {
        let hardware = board_hardware("nrf52840_ble_split");
        let module = syn::parse2(quote! {
            mod keyboard {
                #[Override(chip_config)]
                fn config() -> embassy_nrf::config::Config {
                    let mut config = embassy_nrf::config::Config::default();
                    config.lfclk_source = embassy_nrf::config::LfclkSource::ExternalXtal;
                    config
                }
            }
        })
        .unwrap();
        for id in [None, Some(0)] {
            let tokens = super::expand_chip_init(&hardware, id, &module);
            let names = binding_names(tokens.clone());
            for required in [
                "p",
                "lfclk_cfg",
                "mpsl",
                "rng",
                "sdc_mem",
                "ble_controller",
                "ble_addr",
            ] {
                assert!(
                    names.iter().any(|name| name == required),
                    "missing {required}: {tokens}"
                );
            }
            assert_eq!(names.iter().filter(|name| *name == "p").count(), 1);
            assert!(tokens.to_string().contains("ExternalXtal"));
        }
    }

    #[test]
    fn chip_config_override_preserves_pico_w_ble_initialization() {
        let hardware = board_hardware("pi_pico_w_ble");
        let module = syn::parse2(quote! {
            mod keyboard {
                #[Override(chip_config)]
                fn config() -> embassy_rp::config::Config { embassy_rp::config::Config::default() }
            }
        })
        .unwrap();
        let tokens = super::expand_chip_init(&hardware, None, &module);
        let names = binding_names(tokens.clone());
        assert!(
            names.iter().any(|name| name == "ble_controller"),
            "{tokens}"
        );
        assert!(names.iter().any(|name| name == "ble_addr"), "{tokens}");
    }

    #[test]
    fn mpsl_lfclk_override_composes_with_chip_config_in_either_declaration_order() {
        let hardware = board_hardware("nrf52840_ble_split");
        let chip = quote! {
            #[Override(chip_config)]
            fn chip_config() -> embassy_nrf::config::Config { embassy_nrf::config::Config::default() }
        };
        let mpsl = quote! {
            #[Override(mpsl_lfclk_config)]
            fn lfclk_config() -> nrf_sdc::mpsl::raw::mpsl_clock_lfclk_cfg_t {
                nrf_sdc::mpsl::raw::mpsl_clock_lfclk_cfg_t {
                    source: nrf_sdc::mpsl::raw::MPSL_CLOCK_LF_SRC_XTAL as u8,
                    rc_ctiv: 0, rc_temp_ctiv: 0, accuracy_ppm: 50,
                    skip_wait_lfclk_started: false,
                }
            }
        };
        for items in [quote! { #chip #mpsl }, quote! { #mpsl #chip }] {
            let module = syn::parse2(quote! { mod keyboard { #items } }).unwrap();
            assert!(crate::codegen::override_helper::validate_overwritten_attrs(&module).is_none());
            let tokens = super::expand_chip_init(&hardware, None, &module).to_string();
            assert!(tokens.contains("MPSL_CLOCK_LF_SRC_XTAL"), "{tokens}");
            assert!(!tokens.contains("MPSL_CLOCK_LF_SRC_RC"), "{tokens}");
            assert!(tokens.contains("build_sdc"), "{tokens}");
        }
    }

    #[test]
    fn cfg_disabled_clock_override_keeps_the_default_rc_config() {
        let hardware = board_hardware("nrf52840_ble_split");
        let module = syn::parse2(quote! {
            mod keyboard {
                #[cfg(any())]
                #[Override(mpsl_lfclk_config)]
                fn lfclk_config() { disabled_clock_config() }
            }
        })
        .unwrap();
        assert!(crate::codegen::override_helper::validate_overwritten_attrs(&module).is_none());
        let tokens = super::expand_chip_init(&hardware, None, &module).to_string();
        assert!(tokens.contains("MPSL_CLOCK_LF_SRC_RC"), "{tokens}");
        assert!(!tokens.contains("disabled_clock_config"), "{tokens}");
    }

    #[test]
    fn whole_chip_init_override_still_replaces_generated_initialization() {
        let hardware = board_hardware("nrf52840_ble_split");
        let module = syn::parse2(quote! {
            mod keyboard {
                #[Override(chip_init)]
                fn init() { let custom_peripherals = application_init(); }
            }
        })
        .unwrap();
        let names = binding_names(super::expand_chip_init(&hardware, None, &module));
        assert_eq!(names, ["custom_peripherals"]);
    }

    #[test]
    fn unsupported_mpsl_hook_is_rejected_regardless_of_whole_init_order() {
        let hardware = board_hardware("rp2040");
        let whole_init = quote! {
            #[Override(chip_init)]
            fn init() { let p = application_init(); }
        };
        let mpsl = quote! {
            #[Override(mpsl_lfclk_config)]
            fn lfclk_config() { application_clock_config() }
        };
        for items in [quote! { #whole_init #mpsl }, quote! { #mpsl #whole_init }] {
            let module = syn::parse2(quote! { mod keyboard { #items } }).unwrap();
            let tokens = super::expand_chip_init(&hardware, None, &module).to_string();
            assert!(
                tokens.contains("mpsl_lfclk_config requires an nRF52 BLE keyboard"),
                "{tokens}"
            );
        }
    }

    /// The chip init generated for an nRF52833 keyboard whose `[chip.nrf52833]` holds `chip_section`.
    fn nrf52833_chip_init(name: &str, chip_section: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("rmk-macro-{name}-{}.toml", std::process::id()));
        let toml = format!(
            r#"
[keyboard]
name = "REG0 test"
vendor_id = 0x4c4b
product_id = 0x4643
chip = "nrf52833"

[matrix]
row_pins = ["P0_02"]
col_pins = ["P0_03"]

[layout]
rows = 1
cols = 1

[chip.nrf52833]
{chip_section}
"#
        );
        std::fs::write(&path, toml).unwrap();
        let hardware = KeyboardTomlConfig::new_from_toml_path(&path).hardware();
        std::fs::remove_file(&path).ok();
        chip_init_default(&hardware.unwrap_or_else(|e| panic!("{e}")), None).to_string()
    }

    #[test]
    fn nrf52833_writes_the_configured_reg0_voltage() {
        let tokens = nrf52833_chip_init("reg0-3v3", r#"dcdc_reg0_voltage = "3V3""#);
        let expected =
            quote! { config.dcdc.reg0_voltage = Some(::embassy_nrf::config::Reg0Voltage::_3V3); };
        assert!(tokens.contains(&expected.to_string()), "{tokens}");

        let tokens = nrf52833_chip_init("reg0-1v8", r#"dcdc_reg0_voltage = "1V8""#);
        let expected =
            quote! { config.dcdc.reg0_voltage = Some(::embassy_nrf::config::Reg0Voltage::_1V8); };
        assert!(tokens.contains(&expected.to_string()), "{tokens}");
    }

    #[test]
    fn nrf52833_leaves_reg0_alone_unless_configured() {
        let tokens = nrf52833_chip_init("reg0-unset", "dcdc_reg1 = true");
        assert!(!tokens.contains("reg0_voltage"), "{tokens}");
    }
}
