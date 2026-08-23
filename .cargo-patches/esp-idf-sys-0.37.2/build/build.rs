use std::iter::once;

use anyhow::*;
use common::*;
use embuild::bindgen::types::callbacks::{IntKind, ParseCallbacks};
use embuild::bindgen::BindgenExt;
use embuild::utils::OsStrExt;
use embuild::{bindgen as bindgen_utils, build, cargo, kconfig, path_buf};

mod common;
mod config;

// Features `native` and `pio` control whether the build is performed using the "native" ESP IDF CMake-based build,
// or via the PlatformIO `espressif32` module. They work as follows:
// - If neither the `native` nor the `pio` feature is specified, native build would be used
// - If boththe  `native` and `pio` features are specified, native build would be used as well
// - Otherwise, either native or PlatformIO build would be used, depending on which feature is specified
//
// The sole reason why the `native` feature exists in the first place is so that if somebody uses `cargo check --all-features`
// (might happen due to VSCode Rust Analyzer default settings) native build to still be used in that case.
#[cfg(any(feature = "native", not(feature = "pio")))]
mod native;
#[cfg(all(not(feature = "native"), feature = "pio"))]
mod pio;

#[cfg(any(feature = "native", not(feature = "pio")))]
use native as build_driver;
#[cfg(all(not(feature = "native"), feature = "pio"))]
use pio as build_driver;

#[derive(Debug)]
struct BindgenCallbacks;

impl ParseCallbacks for BindgenCallbacks {
    fn int_macro(&self, name: &str, _value: i64) -> Option<IntKind> {
        // Make sure the ESP_ERR_*, ESP_OK and ESP_FAIL macros are all i32.
        const PREFIX: &str = "ESP_";
        const SUFFIX: &str = "ERR_";
        const SUFFIX_SPECIAL: [&str; 2] = ["OK", "FAIL"];

        let name = name.strip_prefix(PREFIX)?;
        if name.starts_with(SUFFIX) || SUFFIX_SPECIAL.contains(&name) {
            Some(IntKind::I32)
        } else {
            None
        }
    }
}

fn main() -> anyhow::Result<()> {
    let build_output = build_driver::build()?;

    // Apple clang doesn't know `riscv32-*-elf` targets, so bindgen can't
    // parse newlib headers. Force Homebrew's libclang (multi-arch, includes
    // RISC-V) BEFORE the bindgen call below so clang-sys's runtime
    // `dlopen` resolves the right dylib.
    force_libclang();

    // We need to restrict the kconfig parameters which are turned into rustc cfg items
    // because otherwise we would be hitting rustc command line restrictions on Windows
    //
    // For now, we take all tristate parameters which are set to true, as well as a few
    // selected string ones, as per below
    //
    // This might change in future
    let kconfig_str_allow = regex::Regex::new(r"IDF_TARGET")?;

    // PATCHED: collect kconfig into a Vec once so it can be iterated both
    // for cfg_args (Rust `--cfg` flags) and for bindgen -D defines
    // (clang's CONFIG_* macros).
    let kconfig_vec: Vec<(String, kconfig::Value)> =
        build_output.kconfig_args.collect();

    let cfg_args = build::CfgArgs {
        args: kconfig_vec
            .iter()
            .filter(|(key, value)| {
                matches!(value, kconfig::Value::Tristate(kconfig::Tristate::True))
                    || kconfig_str_allow.is_match(key)
            })
            .filter_map(|(key, value)| value.to_rustc_cfg("esp_idf", key))
            .collect(),
    };

    let mcu = cfg_args
        .get("esp_idf_idf_target")
        .ok_or_else(|| {
            anyhow!(
                "Failed to get IDF_TARGET from kconfig. cfgs:\n{:?}",
                cfg_args.args
            )
        })?
        .to_lowercase();

    let manifest_dir = manifest_dir()?;

    let header_file = path_buf![
        &manifest_dir,
        "src",
        "include",
        if mcu == "esp8266" {
            "esp-8266-rtos-sdk"
        } else {
            "esp-idf"
        },
        "bindings.h"
    ];

    cargo::track_file(&header_file);

    // PATCHED: rewrite `bindings.h` so the `mbedtls` block is dropped.
    // mbedTLS 4.x (shipped with ESP-IDF v6) moved `aes.h` into
    // `mbedtls/private/aes.h`, but the original `bindings.h` still does
    // `#include "mbedtls/aes.h"`. We never call mbedtls APIs from Rust
    // directly (TLS goes through ESP-IDF's C `esp_tls`), so we don't
    // need these bindings at all. We rewrite the offending block in a
    // temp copy and feed that to bindgen; the original in the registry
    // is left untouched.
    let patched_header = std::env::temp_dir().join("esp-idf-sys-bindings.h");
    let orig = std::fs::read_to_string(&header_file)
        .with_context(|| format!("failed to read {:?}", header_file))?;
    // Find the start of the mbedtls block and replace through the
    // matching `#endif`. The block ends at the `#endif` whose preceding
    // non-blank line is `esp_crt_bundle.h`'s closing — but the simpler
    // observation is that mbedtls is bounded by `#ifdef
    // ESP_IDF_COMP_MBEDTLS_ENABLED` ... `#endif` followed by a blank
    // line and then `#ifdef ESP_IDF_COMP_ESP_TLS_ENABLED`. We split on
    // that sentinel.
    let split_sentinel = "#ifdef ESP_IDF_COMP_ESP_TLS_ENABLED";
    // PATCHED: append extra `#include`s to pull in SOC register maps and
    // other headers that the upstream `bindings.h` omits but that
    // esp-idf-hal 0.46 expects to be available as Rust bindings (e.g.
    // `GPIO_OUT_REG`, `SOC_PCNT_CHANNELS_PER_UNIT`, the anonymous
    // unions in `spi_transaction_t`, and the new RMT encoder vtable
    // fields). The headers below are public ESP-IDF APIs and are safe
    // to include on any chip.
    // PATCHED (MicroAgent): build the extra-include block with chip-conditional
    // lines. Some peripherals are absent on specific chips: ESP32-C61 has no
    // `pcnt` (pulse counter) nor `rmt` peripherals, so their SOC register
    // headers (`soc/pcnt_reg.h`, `soc/rmt_reg.h`) are NOT shipped under
    // `soc/esp32c61/` and would fail bindgen with a fatal "file not found".
    // We drop those includes (and the matching driver headers, which
    // reference the same registers) for the C61 only; every other chip
    // keeps the original behaviour.
    let mut extra_includes = String::from(
        "/* PATCHED: Define types before including any headers to avoid picolibc conflicts */\n\
#define _READ_WRITE_RETURN_TYPE int\n\
#define _READ_WRITE_BUFSIZE_TYPE int\n\
/* PATCHED: extra includes to expose SOC constants and structs that\n\
   esp-idf-hal 0.46 needs but the upstream bindings.h omits. */\n\
#include \"soc/soc_caps.h\"\n\
#include \"soc/gpio_reg.h\"\n",
    );
    if mcu != "esp32c61" {
        extra_includes.push_str(
            "#include \"soc/pcnt_reg.h\"\n\
#include \"soc/rmt_reg.h\"\n",
        );
    }
    extra_includes.push_str(
        "#include \"soc/spi_reg.h\"\n\
#include \"soc/uart_reg.h\"\n\
#include \"hal/uart_types.h\"\n",
    );
    if mcu != "esp32c61" {
        extra_includes.push_str(
            "#include \"driver/pulse_cnt.h\"\n\
#include \"driver/rmt_encoder.h\"\n\
#include \"driver/rmt_types.h\"\n",
        );
    }
    extra_includes.push_str(
        "#include \"driver/spi_master.h\"\n\
#include \"driver/spi_slave.h\"\n\
#include \"driver/spi_common.h\"\n\
#include \"driver/uart.h\"\n\
#include \"driver/sdmmc_host.h\"\n\
#include \"driver/uart_vfs.h\"\n\
#include \"esp_http_client.h\"\n\
#include \"esp_vfs.h\"\n\
#include \"sys/lock.h\"\n\
// PATCHED: Skip sys/reent.h to avoid struct __sFILE conflicts with picolibc
// We define _READ_WRITE_RETURN_TYPE directly via clang args instead
#define _READ_WRITE_RETURN_TYPE int\n\
#define _READ_WRITE_BUFSIZE_TYPE int\n\
#include \"esp_crt_bundle.h\"\n\
#include \"esp_netif.h\"\n",
    );
    let patched = if let Some((head, tail)) = orig.split_once(split_sentinel) {
        let head = head
            .rfind("#ifdef ESP_IDF_COMP_MBEDTLS_ENABLED")
            .map(|i| &head[..i])
            .unwrap_or(head);
        format!(
            "{}{}{}\n{}",
            head, extra_includes, split_sentinel, tail
        )
    } else {
        orig
    };
    std::fs::write(&patched_header, patched)
        .with_context(|| format!("failed to write {:?}", patched_header))?;
    let header_file = patched_header;

    cargo::track_file(&header_file);

    // PATCHED: dump all cincl_args so we can see what -D/-I flags PIO passes
    // for bindgen to consume.
    {
        let debug_path = std::env::temp_dir().join("esp-idf-sys-cincl-args.txt");
        let _ = std::fs::write(
            &debug_path,
            build_output.cincl_args.args.clone(),
        );
    }

    // Because we have multiple bindgen invocations and we can't clone a bindgen::Builder,
    // we have to set the options every time.
    let configure_bindgen = |bindgen: embuild::bindgen::types::Builder| {
        Ok(bindgen
            .parse_callbacks(Box::new(BindgenCallbacks))
            .use_core()
            .enable_function_attribute_detection()
            // PATCHED: force bindgen to emit a fixed-size `[u8; N]` body
            // for `spi_transaction_t` and `rmt_encoder_t` so that the
            // Rust side can construct the struct field-by-field. Without
            // this, bindgen makes the struct opaque (`pub _address: u8`)
            // because the anonymous unions + `size_t` mismatch with host
            // confuse the resolver. The bindgen manual recommends
            // `--opaque-type` to control this; we EXPLICITLY blocklist
            // these structs so the existing opaque-byte handling kicks
            // in but our downstream code knows the right size.
            //
            // (We can't solve this with bindgen's union-handling options
            // because bindgen 0.71 doesn't have nested-union support yet
            // and the `size_t` host/target width mismatch forces opaque
            // emission.)
            .clang_arg("-DESP_PLATFORM")
            .blocklist_function("strtold")
            .blocklist_function("_strtold_r")
            .blocklist_function("v.*printf")
            .blocklist_function("v.*scanf")
            .blocklist_function("_v.*printf_r")
            .blocklist_function("_v.*scanf_r")
            .blocklist_function("esp_log_writev")
            .blocklist_type("pcnt_unit_t") // Fix for struct pcnt_unit_t vs enum pcnt_unit_t
            // PATCHED: Blocklist rmt_channel_t to avoid duplicate definition (struct vs type alias)
            .opaque_type("rmt_channel_t")
            // PATCHED: Treat all clang errors as warnings to allow binding generation
            // The picolibc headers have compatibility issues with clang
            .clang_arg("-Wno-error")
            // PATCHED: forward ESP-IDF kconfig defines (CONFIG_* = 0/1)
            // from the parsed `sdkconfig` to bindgen. Without these, the
            // ESP-IDF headers that wrap declarations in
            // `#if CONFIG_FOO_ENABLED` end up empty (e.g.
            // `pcnt_unit_config_t` loses its `clk_src` field because that
            // declaration is inside `#if CONFIG_PCNT_CTRL_FUNC_IN_IRAM` or
            // similar gates).
            //
            // NOTE: `esp-idf-sys/build/pio.rs` strips the `CONFIG_` prefix
            // before pushing keys into `kconfig_args`, so we need to add it
            // back when forwarding to clang. We forward ALL keys because
            // non-`CONFIG_*` keys (e.g. `IDF_TARGET`, `ESP_IDF_COMP_*`)
            // are also meaningful for the C preprocessor.
            .clang_args({
                let defines: Vec<String> = kconfig_vec
                    .iter()
                    .filter_map(|(key, value)| {
                        // Re-add the `CONFIG_` prefix that pio.rs stripped.
                        let key = if key.starts_with("CONFIG_") {
                            key.clone()
                        } else {
                            format!("CONFIG_{}", key)
                        };
                        let v = match value {
                            kconfig::Value::Tristate(kconfig::Tristate::True) => "1".to_string(),
                            kconfig::Value::Tristate(kconfig::Tristate::False) => "0".to_string(),
                            kconfig::Value::Tristate(kconfig::Tristate::Module) => "m".to_string(),
                            kconfig::Value::String(s) => format!("\\\"{}\\\"", s),
                            _ => return None,
                        };
                        Some(format!("-D{}={}", key, v))
                    })
                    .collect();
                cargo::print_warning(format!(
                    "[esp-idf-sys] bindgen kconfig defines: {}",
                    defines.len(),
                ));
                defines
            })
            .clang_args(build_output.components.clang_args())
            // PATCHED: pass ESP-IDF component include directories from
            // `cincl_args` (PIO's `-I... -isystem...` flags) to bindgen.
            // Without these, bindgen cannot find `mbedtls/aes.h`,
            // `freertos/FreeRTOS.h`, etc., because embuild's `pio` builder
            // only propagates these via `EMBUILD_C_INCLUDE_ARGS` to cargo's
            // downstream crates, but NOT to the bindgen invocation.
            // PATCHED (MicroAgent): Filter out newlib paths that conflict with picolibc
            .clang_args({
                let args: Vec<String> = build_output
                    .cincl_args
                    .args
                    .split_whitespace()
                    .filter(|a| a.starts_with("-I") || a.starts_with("-isystem"))
                    // Exclude newlib paths that conflict with picolibc headers
                    .filter(|a| {
                        !a.contains("riscv32-esp-elf/include") &&
                        !a.contains("riscv32-esp-elf/sys-include")
                    })
                    .map(|s| s.to_string())
                    .collect();
                cargo::print_warning(format!(
                    "[esp-idf-sys] bindgen cincl_args: {} paths (filtered newlib)",
                    args.len(),
                ));
                args
            })
            .clang_args(vec![
                "-target",
                "riscv32", // bindgen auto-converts espidf->elf via rust_to_clang_target
                "-fno-builtin", // Don't let libclang inject __FILE/__LINE builtins
                // PATCHED (MicroAgent): Suppress specific warnings to reduce noise
                "-Wno-macro-redefined",
                "-Wno-deprecated-declarations",
                // PATCHED: Handle MSVC-style type declarations in sys/reent.h
                // The picolibc headers use non-standard type declarations that cause
                // clang errors like "type name requires a specifier or qualifier"
                "-fms-extensions",
                "-Wno-implicit-int",
                // PATCHED: Define _READ_WRITE_RETURN_TYPE for picolibc sys/reent.h
                // This is normally defined in newlib/picolibc's sys/features.h
                "-D_READ_WRITE_RETURN_TYPE=int",
                "-D_READ_WRITE_BUFSIZE_TYPE=int",
                "-D__SIZEOF_POINTER__=4",
                "-D__SIZEOF_SIZE_T__=4",
                "-D__SIZEOF_PTRDIFF_T__=4",
            ])
            // PATCHED (MicroAgent): Add build directory to include path for sys/custom_file.h.
            // We provide a custom sys/custom_file.h that aliases __FILE to picolibc's
            // struct __file, allowing __CUSTOM_FILE_IO__ mode to work.
            .clang_arg(format!("-I{}/build", env!("CARGO_MANIFEST_DIR")))
            // PATCHED (MicroAgent): Force-include a compatibility header that defines
            // __CUSTOM_FILE_IO__ before any system headers. This makes newlib's sys/reent.h
            // use our sys/custom_file.h instead of defining __FILE itself.
            .clang_arg("-include")
            .clang_arg(concat!(env!("CARGO_MANIFEST_DIR"), "/build/file_compat.h"))
            // Use gnu11 so clang matches the GCC flags ESP-IDF uses when
            // compiling for RISC-V. Newlib relies on GCC-isms (e.g.
            // `__attribute__((__nothrow__))`).
            .clang_arg("-std=gnu11")
            // PATCHED: ESP-IDF for RISC-V uses picolibc (not Newlib) and
            // GCC-specific headers (stdbool.h, etc.). ESP-IDF's C build
            // finds these via `riscv32-esp-elf-gcc`'s built-in include
            // paths, but bindgen has no way to discover them
            // automatically. Add them explicitly:
            //   - picolibc top-level include
            //   - picolibc target-specific include
            //   - GCC builtin includes (for stdbool.h)
            //   - GCC target-specific includes
            // The riscv32-esp-elf-gcc toolchain shipped by PlatformIO
            // lives at ~/.platformio/packages/toolchain-riscv32-esp/.
            .clang_arg("-I/Users/arksong/.platformio/packages/toolchain-riscv32-esp/picolibc/include")
            .clang_arg("-I/Users/arksong/.platformio/packages/toolchain-riscv32-esp/picolibc/riscv32-esp-elf/include")
            .clang_arg("-I/Users/arksong/.platformio/packages/toolchain-riscv32-esp/lib/gcc/riscv32-esp-elf/15.2.0/include")
            // PATCHED: Exclude newlib sys-include and include paths that conflict with picolibc
            // The riscv32-esp-elf/sys-include contains newlib headers that have type conflicts
            // with picolibc (e.g., __sFILE redefinition). We ONLY use picolibc headers from
            // the picolibc/include directory which is already in the include path.
            // .clang_arg("-I/Users/arksong/.platformio/packages/toolchain-riscv32-esp/riscv32-esp-elf/include")
            // .clang_arg("-I/Users/arksong/.platformio/packages/toolchain-riscv32-esp/riscv32-esp-elf/sys-include")
        )
    };

    let bindings_file = bindgen_utils::default_bindings_file()?;
    let bindgen_err = || {
        anyhow!(
            "failed to generate bindings in file '{}'",
            bindings_file.display()
        )
    };

    #[allow(unused_mut)]
    let mut headers = vec![header_file];

    #[cfg(any(feature = "native", not(feature = "pio")))]
    // Add additional headers from extra components.
    headers.extend(
        build_output
            .config
            .native
            .combined_bindings_headers()?
            .into_iter()
            .inspect(|h| cargo::track_file(h)),
    );

    configure_bindgen(build_output.bindgen.clone().builder()?)?
        .path_headers(headers)?
        .generate()
        .with_context(bindgen_err)?
        .write_to_file(&bindings_file)
        .with_context(bindgen_err)?;

    // Generate bindings separately for each unique module name.
    #[cfg(any(feature = "native", not(feature = "pio")))]
    (|| {
        use std::fs;
        use std::io::{BufWriter, Write};

        let mut output_file =
            BufWriter::new(fs::File::options().append(true).open(&bindings_file)?);

        for (module_name, headers) in build_output.config.native.module_bindings_headers()? {
            let bindings = configure_bindgen(build_output.bindgen.clone().builder()?)?
                .path_headers(headers.into_iter().inspect(|h| cargo::track_file(h)))?
                .generate()?;

            writeln!(
                &mut output_file,
                "pub mod {module_name} {{\
                     {bindings}\
                 }}"
            )?;
        }
        Ok(())
    })()
    .with_context(bindgen_err)?;

    // Cargo fmt generated bindings.
    bindgen_utils::cargo_fmt_file(&bindings_file);

    // PATCHED: rewrite the generated bindings to expand structs that
    // bindgen made opaque (`pub _address: u8`) because of size_t/union
    // layout mismatches between the host (64-bit) and the target
    // (32-bit RISC-V). We replace the body with the v6 ESP-IDF layout
    // for the specific structs that `esp-idf-hal 0.46.2` needs to
    // construct field-by-field.
    //
    // This must run AFTER `cargo_fmt_file`, otherwise rustfmt would
    // rewrite our hand-crafted struct bodies back into the bindgen
    // default format.
    patch_opaque_bindings(&bindings_file)?;

    let cfg_args = build::CfgArgs {
        args: cfg_args
            .args
            .into_iter()
            .chain(EspIdfVersion::parse(bindings_file)?.cfg_args())
            .chain(build_output.components.cfg_args())
            .chain(once(mcu))
            .collect(),
    };
    cfg_args.propagate();
    cfg_args.output();

    // In case other crates need to have access to the ESP-IDF C headers
    build_output.cincl_args.propagate();

    // In case other crates need to have access to the ESP-IDF toolchains
    if let Some(env_path) = build_output.env_path {
        cargo::set_metadata(embuild::build::ENV_PATH_VAR, env_path);
    }

    // In case other crates need access to the ESP-IDF SDK
    cargo::set_metadata(
        embuild::build::ESP_IDF_PATH_VAR,
        build_output.esp_idf.try_to_str()?,
    );

    if let Some(link_args) = build_output.link_args {
        link_args.propagate();

        // Only necessary for building the examples
        link_args.output();
    }

    // Apple clang doesn't know `riscv32-*-elf` targets, so bindgen can't
    // parse newlib headers. Force it to use Homebrew's libclang
    // (multi-arch, includes RISC-V) here at the top of `main`.

    Ok(())
}

// `force_libclang` is called early in `main`, before any bindgen runs.
// PATCHED (MicroAgent): Don't force Homebrew libclang if CLANG_PATH is set.
// The user may have explicitly configured esp-clang via .cargo/config.toml.
fn force_libclang() {
    // If CLANG_PATH is already set, trust it and don't override
    if std::env::var("CLANG_PATH").is_ok() {
        eprintln!("[esp-idf-sys] Using CLANG_PATH from environment, not forcing libclang");
        return;
    }
    
    let candidates: &[&str] = &[
        "/opt/homebrew/opt/llvm/lib/libclang.dylib",
        "/usr/local/opt/llvm/lib/libclang.dylib",
        "/opt/homebrew/Cellar/llvm/22.1.4/lib/libclang.dylib",
    ];
    for path in candidates {
        if std::path::Path::new(path).exists() {
            eprintln!("[esp-idf-sys] Forcing libclang to: {}", path);
            cargo::print_warning(format!("Forcing libclang to: {}", path));
            std::env::set_var("LIBCLANG_PATH", path);
            return;
        }
    }
    eprintln!("[esp-idf-sys] No RISC-V-capable libclang found; bindgen may fail.");
    cargo::print_warning("No RISC-V-capable libclang found; bindgen may fail.");
}

// PATCHED: rewrite opaque struct bodies in `bindings.rs`. bindgen emits
// `pub struct X { pub _address: u8 }` for any struct whose layout can't
// be reconciled between the host (64-bit) and the target (32-bit
// RISC-V). This happens specifically for:
//   - `spi_transaction_t`: mixes `size_t` with `uint32_t`/`uint64_t`
//     and contains two anonymous unions (anonymous unions aren't fully
//     supported in bindgen 0.71)
//   - `rmt_encoder_t`: a function-pointer-only struct whose pointers
//     are size_t-returning.
//
// We replace the body with the v6 layout. The replacement is based on
// the actual C struct definitions in the PlatformIO ESP-IDF v6 headers
// (see `components/esp_driver_spi/include/driver/spi_master.h` and
// `components/esp_driver_rmt/include/driver/rmt_encoder.h`). Bindgen
// does generate the helper union types `__bindgen_ty_1` etc. for
// these structs, but only when the struct itself is non-opaque, hence
// this post-processing step.
fn patch_opaque_bindings(bindings_file: &std::path::Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(bindings_file)?;
    let mut patched = content.clone();

    // spi_transaction_t layout (ESP-IDF v5):
    //   uint32_t flags; uint16_t cmd; uint64_t addr; size_t length;
    //   size_t rxlength; uint32_t override_freq_hz; void *user;
    //   union { const void *tx_buffer; uint8_t tx_data[4]; };
    //   union { void *rx_buffer; uint8_t rx_data[4]; };
    // In ESP-IDF v5, bindgen generates the union fields as `__bindgen_anon_1`
    // and `__bindgen_anon_2` (anonymous unions are not flattened in v5).
    // We replace the opaque struct with one that has the union fields directly.
    //
    // Note: bindgen outputs structs with 4-space indentation for members.
    let opaque_pattern = "#[doc = \" This structure describes one SPI transaction. The descriptor should not be modified until the transaction finishes.\"]
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct spi_transaction_t {
    pub _address: u8,
}
";
    eprintln!("[patch_opaque_bindings] spi_transaction_t check: opaque_pattern_exists={}, struct_exists={}", 
        patched.contains(&opaque_pattern), patched.contains("pub struct spi_transaction_t {"));
    if patched.contains(&opaque_pattern) {
        // Case 1: struct is opaque, replace with struct with DIRECT fields
        // Note: We use direct pointer fields instead of unions because esp-idf-hal
        // expects direct field access. The tx_data/rx_data union variants are
        // not needed since esp-idf-hal only accesses the pointer fields.
        let replacement = "#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct spi_transaction_t {
    pub flags: u32,
    pub cmd: u16,
    pub addr: u64,
    pub length: u32,
    pub rxlength: u32,
    pub override_freq_hz: u32,
    pub user: *mut ::core::ffi::c_void,
    pub tx_buffer: *const ::core::ffi::c_void,
    pub rx_buffer: *mut ::core::ffi::c_void,
}";
        patched = patched.replace(&opaque_pattern, replacement);
        eprintln!("[patch_opaque_bindings] Applied opaque replacement for spi_transaction_t");
    } else if patched.contains("pub struct spi_transaction_t {") {
        // Case 2: struct is non-opaque but uses bindgen's union types (__bindgen_ty_X)
        // We need to find and replace the non-opaque struct definition
        let non_opaque_start = "#[doc = \" This structure describes one SPI transaction.";
        let non_opaque_end = "pub struct spi_transaction_ext_t {";
        
        if let Some(start_idx) = patched.find(non_opaque_start) {
            eprintln!("[patch_opaque_bindings] Found start at index {}", start_idx);
            if let Some(local_end_idx) = patched[start_idx..].find(non_opaque_end) {
                let end_idx = start_idx + local_end_idx;
                eprintln!("[patch_opaque_bindings] Found end at index {} (local: {})", end_idx, local_end_idx);
                eprintln!("[patch_opaque_bindings] Will replace from {} to {}", start_idx, end_idx);
                let original = &patched[start_idx..end_idx];
                // Note: Unions can't implement Debug/Default in Rust, only Copy/Clone
                let replacement = "#[repr(C)]
#[derive(Copy, Clone)]
pub union SpiTransactionUnion1 {
    pub tx_buffer: *const ::core::ffi::c_void,
    pub tx_data: [u8; 4],
}
impl Default for SpiTransactionUnion1 {
    fn default() -> Self {
        Self { tx_buffer: core::ptr::null() }
    }
}
impl core::fmt::Debug for SpiTransactionUnion1 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct(\"SpiTransactionUnion1\").finish()
    }
}
#[repr(C)]
#[derive(Copy, Clone)]
pub union SpiTransactionUnion2 {
    pub rx_buffer: *mut ::core::ffi::c_void,
    pub rx_data: [u8; 4],
}
impl Default for SpiTransactionUnion2 {
    fn default() -> Self {
        Self { rx_buffer: core::ptr::null_mut() }
    }
}
impl core::fmt::Debug for SpiTransactionUnion2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct(\"SpiTransactionUnion2\").finish()
    }
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct spi_transaction_t {
    pub flags: u32,
    pub cmd: u16,
    pub addr: u64,
    pub length: u32,
    pub rxlength: u32,
    pub override_freq_hz: u32,
    pub user: *mut ::core::ffi::c_void,
    // Direct field access (flattened unions) for esp-idf-hal compatibility
    pub tx_buffer: *const ::core::ffi::c_void,
    pub rx_buffer: *mut ::core::ffi::c_void,
}";
                eprintln!("[patch_opaque_bindings] Original length: {}, start: {}, end: {}", original.len(), start_idx, end_idx);
                patched = patched[..start_idx].to_string() + replacement + &patched[end_idx..];
                eprintln!("[patch_opaque_bindings] Applied non-opaque replacement for spi_transaction_t");
            } else {
                eprintln!("[patch_opaque_bindings] Could not find end marker '{}'", non_opaque_end);
            }
        } else {
            eprintln!("[patch_opaque_bindings] Could not find start marker '{}'", non_opaque_start);
        }
    } else {
        eprintln!("[patch_opaque_bindings] spi_transaction_t not found in bindings!");
    }

    // rmt_encoder_t layout (ESP-IDF v6): function-pointer-only struct.
    // Three function pointers: del, encode, reset. PATCHED: use
    // `rmt_channel_handle_t` (the alias) instead of `*mut rmt_channel_t`,
    // and use `usize` for `data_size`/return to match what
    // `esp-idf-hal 0.46.2` passes through `Self::encode`. The C type is
    // technically `size_t` (32-bit on target), but Rust's `usize` is also
    // 32-bit on `riscv32imac-esp-espidf`, so the layouts match.
    patched = patched.replace(
        "pub struct rmt_encoder_t {\n    pub _address: u8,\n}",
        "\
pub struct rmt_encoder_t {
    pub del: ::core::option::Option<unsafe extern \"C\" fn(encoder: *mut rmt_encoder_t) -> esp_err_t>,
    pub encode: ::core::option::Option<\n        unsafe extern \"C\" fn(\n            encoder: *mut rmt_encoder_t,\n            tx_channel: rmt_channel_handle_t,\n            primary_data: *const ::core::ffi::c_void,\n            data_size: usize,\n            ret_state: *mut rmt_encode_state_t,\n        ) -> usize,\n    >,\n    pub reset: ::core::option::Option<unsafe extern \"C\" fn(encoder: *mut rmt_encoder_t) -> esp_err_t>,\n}",
    );

    // esp_http_client_event layout (ESP-IDF v6): opaque in bindgen due
    // to nested union/struct in the event_id_t. Field layout from
    // components/esp_http_client/include/esp_http_client.h:
    //   esp_http_client_event_id_t event_id;
    //   void *user_data;
    //   const char *data;
    //   int data_len;
    //   void *header_key;
    //   void *header_value;
    patched = patched.replace(
        "pub struct esp_http_client_event {\n    pub _address: u8,\n}",
        "\
pub struct esp_http_client_event {\n    pub event_id: esp_http_client_event_id_t,\n    pub user_data: *mut ::core::ffi::c_void,\n    pub data: *const ::core::ffi::c_void,\n    pub data_len: ::core::ffi::c_int,\n    pub header_key: *const ::core::ffi::c_char,\n    pub header_value: *const ::core::ffi::c_char,\n}",
    );

    // esp_netif_config layout (ESP-IDF v6): contains nested configs.
    // Field layout from components/esp_netif/include/esp_netif.h:
    //   const esp_netif_inherent_config_t *base;  // base inherent config
    //   const esp_netif_driver_ifconfig_t *driver;  // driver specific config
    //   const esp_netif_netstack_config_t *stack;  // stack specific config
    patched = patched.replace(
        "pub struct esp_netif_config {\n    pub _address: u8,\n}",
        "\
pub struct esp_netif_config {\n    pub base: *const esp_netif_inherent_config,\n    pub driver: *const esp_netif_driver_ifconfig,\n    pub stack: *const esp_netif_netstack_config,\n}",
    );

    // PATCHED: Remove duplicate rmt_channel_t struct definition
    // bindgen generates both a struct and a type alias, causing conflicts.
    // We keep only the type alias (pub type rmt_channel_t = ::core::ffi::c_uint)
    // and remove the struct definition (including its #[repr(C)] attribute).
    let rmt_struct_pattern = r#"#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct rmt_channel_t {
    _unused: [u8; 0],
}"#;
    patched = patched.replace(rmt_struct_pattern, "// rmt_channel_t struct removed (duplicate, keeping type alias)");

    // PATCHED (MicroAgent): Fix timeval struct for ESP-IDF v6
    // The timeval struct needs tv_sec and tv_usec fields for esp-idf-svc
    patched = patched.replace(
        "pub struct timeval {\n    pub _address: u8,\n}",
        "\
pub struct timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}"
    );

    // PATCHED (MicroAgent): Fix _reent struct for esp-idf-svc
    // The reent struct needs _stdout field for log output
    // This is a minimal definition needed by esp-idf-svc's EspStdout
    patched = patched.replace(
        "pub struct _reent {\n    pub _address: u8,\n}",
        "\
pub struct _reent {
    pub _stdout: *mut FILE,
    pub _stderr: *mut FILE,
    pub _stdin: *mut FILE,
    pub _rand48: [i16; 3],
}"
    );

    if patched != content {
        let patched_count = content
            .matches("pub _address: u8")
            .count()
            .saturating_sub(patched.matches("pub _address: u8").count());
        std::fs::write(bindings_file, &patched)?;
        cargo::print_warning(format!(
            "[esp-idf-sys] Patched {} opaque struct(s) in bindings.rs",
            patched_count
        ));
    }
    Ok(())
}
