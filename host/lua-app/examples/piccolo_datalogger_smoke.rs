//! Run with: `cargo run -p magent-lua --example piccolo_datalogger_smoke --features piccolo --no-default-features`
//!
//! This smoke test exercises the piccolo-specific VM in isolation; it requires
//! the `piccolo` feature.

#[cfg(feature = "piccolo")]
mod inner {
    use std::sync::{Arc, Mutex};

    use magent_lua::hardware::SimHardware;
    use magent_lua::piccolo_vm::PiccoloVm;
    use magent_lua::{install_mock_agent, SharedHardware};

    pub fn run() {
        let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
        let agent = install_mock_agent("BLE_SEND:ok").unwrap();
        let mut vm = PiccoloVm::new(hw, agent);

        // Check: is string.format registered?
        let script = r#"
        return type(string.format)
        "#;
        println!("check type...");
        match vm.run_script(script) {
            Ok(_) => println!("OK"),
            Err(e) => println!("ERR: {e}"),
        }

        // Try calling string.format directly via run_script (top-level)
        let script2 = r#"return string.format("hello %s", "world")"#;
        println!("\ncall format via run_script...");
        match vm.run_script(script2) {
            Ok(_) => println!("OK"),
            Err(e) => println!("ERR: {e}"),
        }

        // Try via a Lua function
        let script3 = r#"
        function test()
            return string.format("hello %s", "world")
        end
        return test()
        "#;
        println!("\ncall format via function in run_script...");
        match vm.run_script(script3) {
            Ok(_) => println!("OK"),
            Err(e) => println!("ERR: {e}"),
        }

        println!("\ndone");
    }
}

fn main() {
    #[cfg(feature = "piccolo")]
    {
        inner::run();
    }
    #[cfg(not(feature = "piccolo"))]
    {
        eprintln!("piccolo_datalogger_smoke requires the `piccolo` feature");
        std::process::exit(1);
    }
}
