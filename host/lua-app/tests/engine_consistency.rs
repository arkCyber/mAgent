//! Cross-engine consistency probes — exercise `LuaEngine::engine_name()` and
//! `assert_engine_output()`. The same scripts must produce identical results
//! on `mlua` and `piccolo`.
//!
//! Run with:
//!   cargo test  -p magent-lua --test engine_consistency
//!   cargo test  -p magent-lua --test engine_consistency \
//!       --features piccolo --no-default-features

use std::sync::{Arc, Mutex};

use magent_lua::hardware::SimHardware;
use magent_lua::{assert_engine_output, engine_name, install_mock_agent, LuaEngine};

#[cfg(feature = "mlua")]
use magent_lua::LuaVm;

#[cfg(feature = "piccolo")]
use magent_lua::piccolo_vm::PiccoloVm;

/// Every LuaEngine impl must report a non-empty static name.
#[test]
fn engine_name_is_stable() {
    #[cfg(feature = "mlua")]
    {
        let hw: magent_lua::SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        let agent = install_mock_agent("OK").unwrap();
        let vm = LuaVm::new(hw, agent).unwrap();
        assert_eq!(vm.engine_name(), "mlua");
        assert_eq!(engine_name(&vm), "mlua");
    }

    #[cfg(feature = "piccolo")]
    {
        let hw: magent_lua::SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        let agent = install_mock_agent("OK").unwrap();
        let vm = PiccoloVm::new(hw, agent);
        assert_eq!(vm.engine_name(), "piccolo");
        assert_eq!(engine_name(&vm), "piccolo");
    }
}

/// `assert_engine_output` should produce identical error messages on either
/// engine (engine name is the only difference).
#[test]
fn assert_helper_uses_engine_name_in_message() {
    let expected = "WRONG_EXPECTED_VALUE";
    let script = "function echo(s) return s end";

    #[cfg(feature = "mlua")]
    {
        let hw: magent_lua::SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        let agent = install_mock_agent("OK").unwrap();
        let mut vm = LuaVm::new(hw, agent).unwrap();
        vm.run_script(script).unwrap();
        let err = assert_engine_output(&mut vm, "echo", &["hello"], expected)
            .expect_err("should fail");
        assert!(
            err.to_string().contains("[mlua]"),
            "expected error to be tagged [mlua], got: {err}"
        );
    }

    #[cfg(feature = "piccolo")]
    {
        let hw: magent_lua::SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        let agent = install_mock_agent("OK").unwrap();
        let mut vm = PiccoloVm::new(hw, agent);
        vm.run_script(script).unwrap();
        let err = assert_engine_output(&mut vm, "echo", &["hello"], expected)
            .expect_err("should fail");
        assert!(
            err.to_string().contains("[piccolo]"),
            "expected error to be tagged [piccolo], got: {err}"
        );
    }
}

/// Same Lua script, both engines, same result — the cross-engine invariant
/// that `apps_tests` implicitly relies on, made explicit.
#[test]
fn same_script_same_result_on_either_engine() {
    let script = "function add(a, b) return a .. '+' .. b end";

    #[cfg(feature = "mlua")]
    {
        let hw: magent_lua::SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        let agent = install_mock_agent("OK").unwrap();
        let mut vm = LuaVm::new(hw, agent).unwrap();
        vm.run_script(script).unwrap();
        assert_engine_output(&mut vm, "add", &["2", "3"], "2+3").unwrap();
    }

    #[cfg(feature = "piccolo")]
    {
        let hw: magent_lua::SharedHardware =
            Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
        let agent = install_mock_agent("OK").unwrap();
        let mut vm = PiccoloVm::new(hw, agent);
        vm.run_script(script).unwrap();
        assert_engine_output(&mut vm, "add", &["2", "3"], "2+3").unwrap();
    }
}
