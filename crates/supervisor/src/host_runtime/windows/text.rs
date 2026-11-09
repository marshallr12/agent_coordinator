//! The text a Windows process creation takes: UTF-16 strings, one command
//! line built from an argument list, and an environment block that holds
//! exactly the allowed variables. Nothing here calls the operating system.
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

/// `text` as NUL-terminated UTF-16. An interior NUL would cut the string
/// short where Windows reads it, so it is refused.
pub fn wide(text: &str) -> Result<Vec<u16>> {
    ensure!(!text.contains('\0'), "text holds a NUL character");
    Ok(text.encode_utf16().chain([0]).collect())
}

/// The command line for `argv`, quoted so the C runtime and
/// `CommandLineToArgvW` split it back into exactly these arguments. The
/// launcher never passes a caller's string through a shell.
pub fn command_line(argv: &[String]) -> Result<Vec<u16>> {
    ensure!(!argv.is_empty(), "a launch needs a program to run");
    let quoted: Vec<String> = argv.iter().map(|argument| quote(argument)).collect();
    wide(&quoted.join(" "))
}

/// One argument in the form the C runtime parses: quoted when it is empty
/// or holds white space, with every backslash run that precedes a quote
/// doubled and the quote itself escaped.
fn quote(argument: &str) -> String {
    let needs_quotes = argument.is_empty() || argument.contains([' ', '\t']);
    let mut out = String::new();
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            other => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(other);
                backslashes = 0;
            }
        }
    }
    // A trailing run sits before the closing quote, so it is doubled there.
    let trailing = if needs_quotes {
        backslashes * 2
    } else {
        backslashes
    };
    out.push_str(&"\\".repeat(trailing));
    if needs_quotes {
        format!("\"{out}\"")
    } else {
        out
    }
}

/// A Unicode environment block holding exactly `values`, sorted the way
/// Windows expects, and nothing the launcher itself inherited.
pub fn environment_block(values: &BTreeMap<String, String>) -> Result<Vec<u16>> {
    let mut entries = Vec::new();
    for (name, value) in values {
        ensure!(
            !name.is_empty() && !name.contains(['=', '\0']) && !value.contains('\0'),
            "environment variable {name:?} cannot be passed to a launch"
        );
        entries.push((name.to_uppercase(), format!("{name}={value}")));
    }
    entries.sort();
    let mut block: Vec<u16> = Vec::new();
    for (_, entry) in &entries {
        block.extend(entry.encode_utf16().chain([0]));
    }
    // An empty block still needs its own terminator before the final one.
    block.extend(if entries.is_empty() {
        vec![0, 0]
    } else {
        vec![0]
    });
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command line for `argv`, as text without its terminator.
    fn line(argv: &[&str]) -> String {
        let argv: Vec<String> = argv.iter().map(ToString::to_string).collect();
        let wide = command_line(&argv).unwrap();
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    }

    #[test]
    fn arguments_are_quoted_as_the_c_runtime_parses_them() {
        assert_eq!(line(&["a.exe", "plain"]), "a.exe plain");
        assert_eq!(
            line(&["a.exe", "two words", ""]),
            "a.exe \"two words\" \"\""
        );
        assert_eq!(line(&["a.exe", "say \"hi\""]), "a.exe \"say \\\"hi\\\"\"");
        assert_eq!(line(&["a.exe", "C:\\dir\\"]), "a.exe C:\\dir\\");
        assert_eq!(line(&["a.exe", "C:\\my dir\\"]), "a.exe \"C:\\my dir\\\\\"");
        assert_eq!(line(&["a.exe", "a\\\"b"]), "a.exe a\\\\\\\"b");
    }

    #[test]
    fn a_launch_without_a_program_or_with_a_nul_is_refused() {
        assert!(command_line(&[]).is_err());
        assert!(command_line(&["a\0b".to_owned()]).is_err());
        assert!(wide("a\0").is_err());
    }

    #[test]
    fn the_environment_block_holds_exactly_the_allowed_variables() {
        let values = BTreeMap::from([
            ("b".to_owned(), "2".to_owned()),
            ("A".to_owned(), "1".to_owned()),
        ]);
        let block = environment_block(&values).unwrap();
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "A=1\0b=2\0\0");
        assert_eq!(environment_block(&BTreeMap::new()).unwrap(), [0, 0]);
    }

    #[test]
    fn a_variable_that_could_smuggle_another_is_refused() {
        for (name, value) in [("", "x"), ("A=B", "x"), ("A", "x\0B=y"), ("A\0", "x")] {
            let values = BTreeMap::from([(name.to_owned(), value.to_owned())]);
            assert!(environment_block(&values).is_err(), "{name:?}={value:?}");
        }
    }
}
