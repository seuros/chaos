use super::generated_hook_schemas;
use pretty_assertions::assert_eq;

#[test]
fn loads_generated_hook_schemas() {
    let schemas = generated_hook_schemas();

    assert_eq!(schemas.session_start_command_input["type"], "object");
    assert_eq!(schemas.session_start_command_output["type"], "object");
    assert_eq!(schemas.stop_command_input["type"], "object");
    assert_eq!(schemas.stop_command_output["type"], "object");
}
