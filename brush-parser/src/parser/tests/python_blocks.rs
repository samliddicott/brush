//! Tests for native `PYTHON ... END_PYTHON` block preprocessing.

use super::parse_with_config;
use super::parser_configs;
use anyhow::Result;

#[test]
fn parse_python_block_basic() -> Result<()> {
    let input = "PYTHON\nprint(42)\nEND_PYTHON\n";

    for config in parser_configs() {
        parse_with_config(input, &config)?;
    }

    Ok(())
}

#[test]
fn parse_python_block_with_options_redirection_and_pipe() -> Result<()> {
    let input = "PYTHON -x -v out 2>err.log | cat\nprint(\"ok\")\nEND_PYTHON\n";

    for config in parser_configs() {
        parse_with_config(input, &config)?;
    }

    Ok(())
}

#[test]
fn parse_pipeline_into_python_block() -> Result<()> {
    let input = "echo frompipe | PYTHON -x\nprint(\"ok\")\nEND_PYTHON\n";

    for config in parser_configs() {
        parse_with_config(input, &config)?;
    }

    Ok(())
}

#[test]
fn parse_python_block_in_middle_of_pipeline() -> Result<()> {
    let input = "echo left | PYTHON -x | cat\nprint(\"ok\")\nEND_PYTHON\n";

    for config in parser_configs() {
        parse_with_config(input, &config)?;
    }

    Ok(())
}

#[test]
fn unterminated_python_block_errors() {
    let input = "PYTHON\nprint('missing end')\n";

    for config in parser_configs() {
        let err = parse_with_config(input, &config).expect_err("should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("unterminated PYTHON block") || msg.contains("missing END_PYTHON"),
            "unexpected error for {:?}: {}",
            config.name,
            msg
        );
    }
}
