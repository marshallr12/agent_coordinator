//! The command form native Claude Code hands its shell prefix on Windows,
//! and the candidate command it becomes (phase 5, step 5a).
//!
//! Recorded for Claude Code 2.1.325 on MiniAir (design doc, "Native Claude
//! Code command paths, 2026-11-14"): Git Bash runs the prefix with exactly
//! one argument,
//!
//! ```text
//! source <snapshot> 2>/dev/null || true && export TEMP='<temp>' TMP='<temp>'
//!   && <resets> && eval '<command>' < /dev/null && pwd -P >| <cwd file>
//! ```
//!
//! (one line). Only `<command>` reaches the candidate: the snapshot, the
//! reviewer's temp and the cwd file stay with the shim, which owns the cwd
//! file. An argument that is not exactly this form, a hook command among
//! them, is refused before anything runs. Nothing here touches the
//! operating system, so every platform tests it.

/// The shell option and alias resets between the export and the eval,
/// exactly as recorded.
const RESETS: &str = concat!(
    r"{ \builtin shopt -u extglob || \builtin setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL ",
    r"NO_BRACE_CCL NO_MAGIC_EQUAL_SUBST; } >/dev/null 2>&1 || true && ",
    r"{ \builtin unalias -- 'which'; \builtin unset -f -- 'which'; } >/dev/null 2>&1 || true && ",
    r"{ \builtin unalias -- 'unsetenv'; \builtin unset -f -- 'unsetenv'; } >/dev/null 2>&1 || true",
);

/// Characters an unquoted word may hold. Anything else outside quotes could
/// be shell syntax, so it ends the match.
const BARE: &str = "/._:~+,=@%-";

/// Why an argument is refused; the shim prints it.
pub const NOT_THE_FORM: &str =
    "the command is not in the recorded form of the pinned Claude Code version";
/// The cwd file is not where the harness keeps it.
pub const CWD_OUTSIDE: &str = "the harness cwd file is not in the reviewer's temp";

/// What the shim keeps from one recorded command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    /// The command the harness asked for, unquoted; the only part that runs.
    pub command: String,
    /// The reviewer's temp as the harness exported it (Windows form).
    pub temp: String,
    /// The cwd file as the harness named it (Git Bash form).
    pub cwd_file: String,
}

/// Matches `argument` against the recorded form, refusing anything else.
pub fn parse(argument: &str) -> Result<Wrapped, &'static str> {
    (!argument.contains('\0'))
        .then_some(())
        .ok_or(NOT_THE_FORM)?;
    let rest = expect(argument, "source ")?;
    let (_snapshot, rest) = word(rest)?;
    let rest = expect(rest, " 2>/dev/null || true && export TEMP=")?;
    let (temp, rest) = word(rest)?;
    let (tmp, rest) = word(expect(rest, " TMP=")?)?;
    let rest = expect(expect(expect(rest, " && ")?, RESETS)?, " && eval ")?;
    let (command, rest) = word(rest)?;
    let (cwd_file, rest) = word(expect(rest, " < /dev/null && pwd -P >| ")?)?;
    let complete = rest.is_empty() && tmp == temp && !temp.is_empty();
    complete.then_some(()).ok_or(NOT_THE_FORM)?;
    Ok(Wrapped {
        command,
        temp,
        cwd_file,
    })
}

/// `text` without the leading `literal`, or a refusal.
fn expect<'a>(text: &'a str, literal: &str) -> Result<&'a str, &'static str> {
    text.strip_prefix(literal).ok_or(NOT_THE_FORM)
}

/// One shell word at the start of `text`, decoded, and what follows it. A
/// word is a run of bare characters, single-quoted text, a backslash and the
/// character it escapes, and double-quoted text holding nothing the shell
/// would expand; it ends at a space or the end. Anything else is refused.
fn word(text: &str) -> Result<(String, &str), &'static str> {
    let mut decoded = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(at, c)) = chars.peek() {
        match c {
            ' ' => return finish(decoded, &text[at..]),
            '\'' => quoted(&mut chars, &mut decoded, '\'')?,
            '"' => quoted(&mut chars, &mut decoded, '"')?,
            '\\' => {
                chars.next();
                decoded.push(chars.next().ok_or(NOT_THE_FORM)?.1);
            }
            c if c.is_ascii_alphanumeric() || BARE.contains(c) => {
                decoded.push(c);
                chars.next();
            }
            _ => return Err(NOT_THE_FORM),
        }
    }
    finish(decoded, "")
}

/// A word that decoded to something, and the text after it.
fn finish(decoded: String, rest: &str) -> Result<(String, &str), &'static str> {
    (!decoded.is_empty())
        .then_some((decoded, rest))
        .ok_or(NOT_THE_FORM)
}

/// Consumes one quoted run opened by `quote` into `decoded`. Double quotes
/// may not hold `$`, a backquote or a backslash, which the shell would
/// interpret; single quotes hold anything but themselves.
fn quoted(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    decoded: &mut String,
    quote: char,
) -> Result<(), &'static str> {
    chars.next();
    for (_, c) in chars.by_ref() {
        if c == quote {
            return Ok(());
        }
        if quote == '"' && "$`\\".contains(c) {
            return Err(NOT_THE_FORM);
        }
        decoded.push(c);
    }
    Err(NOT_THE_FORM)
}

/// The cwd file's path in Windows form, when it is one of the harness's
/// layouts inside `temp` (the shim's own record of the reviewer's temp, in
/// Windows form): a flat name, or `claude-<digits>/cwd-<id>` as on Linux.
pub fn cwd_file(wrapped: &Wrapped, temp: &str) -> Result<String, &'static str> {
    if !wrapped.temp.eq_ignore_ascii_case(temp) {
        return Err(CWD_OUTSIDE);
    }
    let root = git_bash_path(temp).ok_or(CWD_OUTSIDE)?;
    let name = wrapped.cwd_file.strip_prefix(&root);
    let name = name.and_then(|n| n.strip_prefix('/')).ok_or(CWD_OUTSIDE)?;
    harness_layout(name).then_some(()).ok_or(CWD_OUTSIDE)?;
    let temp = temp.trim_end_matches('\\');
    Ok(format!("{temp}\\{}", name.replace('/', "\\")))
}

/// `C:\a\b` as Git Bash writes it, `/c/a/b`; `None` for anything that is
/// not a plain absolute path on a drive.
fn git_bash_path(windows: &str) -> Option<String> {
    let mut parts = windows.trim_end_matches('\\').split('\\');
    let drive = parts.next()?.strip_suffix(':')?;
    let letter = drive.chars().next().filter(char::is_ascii_alphabetic)?;
    let parts: Vec<&str> = parts.collect();
    let plain = drive.len() == 1 && parts.iter().all(|p| plain_component(p, false));
    plain.then(|| format!("/{}/{}", letter.to_ascii_lowercase(), parts.join("/")))
}

/// Whether `name` is a path component that names nothing but itself; with
/// `strict` only the characters the harness itself uses.
fn plain_component(name: &str, strict: bool) -> bool {
    let allowed =
        |c: char| c.is_ascii_alphanumeric() || "._-".contains(c) || (!strict && " ()".contains(c));
    !name.is_empty() && name != "." && name != ".." && name.chars().all(allowed)
}

/// The harness's cwd-file layouts below its temp, as the Linux
/// `cwd_guard` admits them.
fn harness_layout(name: &str) -> bool {
    match name.split_once('/') {
        None => plain_component(name, true),
        Some((directory, file)) => {
            let uid = directory.strip_prefix("claude-").unwrap_or("");
            let id = file.strip_prefix("cwd-").unwrap_or("");
            let digits = !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit());
            let id_ok = !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b));
            digits && id_ok
        }
    }
}

/// What the candidate's shell runs: the harness's command and
/// nothing of the reviewer's, with standard input closed as the harness
/// asked. The candidate's directory and environment are the runner's.
pub fn candidate_script(command: &str) -> String {
    format!("eval {} < /dev/null", single_quoted(command))
}

/// `text` as one single-quoted POSIX shell word.
pub fn single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMP: &str = r"C:\Users\role\AppData\Local\Temp";
    const SNAPSHOT: &str = "/c/Users/role/.claude/shell-snapshots/snapshot-bash-1-x.sh";

    /// The recorded argument with `command` (already quoted) and `cwd`.
    fn recorded(command: &str, cwd: &str) -> String {
        format!(
            "source {SNAPSHOT} 2>/dev/null || true && export TEMP='{TEMP}' TMP='{TEMP}' && \
             {RESETS} && eval {command} < /dev/null && pwd -P >| {cwd}"
        )
    }

    const CWD: &str = "/c/Users/role/AppData/Local/Temp/claude-884c-cwd";

    #[test]
    fn the_observed_argument_is_accepted_and_only_its_command_is_kept() {
        // Verbatim from the MiniAir probe log, apart from the account name.
        let wrapped = parse(&recorded("'echo reviewer-route'", CWD)).unwrap();
        assert_eq!(wrapped.command, "echo reviewer-route");
        assert_eq!(wrapped.temp, TEMP);
        assert_eq!(wrapped.cwd_file, CWD);
    }

    #[test]
    fn quotes_inside_the_command_decode_in_either_escape() {
        for quoted in [r"'echo '\''a b'\'''", r#"'echo '"'"'a b'"'"''"#] {
            let wrapped = parse(&recorded(quoted, CWD)).unwrap();
            assert_eq!(wrapped.command, "echo 'a b'", "{quoted}");
        }
        let multiline = parse(&recorded("'cargo test\nexit 3'", CWD)).unwrap();
        assert_eq!(multiline.command, "cargo test\nexit 3");
    }

    #[test]
    fn anything_but_the_recorded_form_is_refused() {
        let good = recorded("'true'", CWD);
        let mut bad = vec![
            String::new(),
            "true".into(),
            "echo hook".into(),
            good.replace("source ", "source  "),
            good.replace("TMP='", "TMP='D:"),
            good.replace("which", "what"),
            good.replace(" < /dev/null", ""),
            good.replace("eval 'true'", "eval $(true)"),
            good.replace("eval 'true'", "eval \"$X\""),
            good.replace("eval 'true'", "eval 'true"),
            format!("{good} ; touch x"),
            format!("{good} x"),
            format!("{good}\0"),
            good.replace(CWD, "$(touch x)"),
        ];
        bad.push(good.replace(SNAPSHOT, "a;b"));
        for argument in bad {
            assert_eq!(parse(&argument), Err(NOT_THE_FORM), "{argument:?}");
        }
    }

    #[test]
    fn the_cwd_file_must_be_a_harness_layout_in_the_reviewers_temp() {
        let wrapped = |cwd: &str| parse(&recorded("'true'", cwd)).unwrap();
        assert_eq!(
            cwd_file(&wrapped(CWD), TEMP).unwrap(),
            format!(r"{TEMP}\claude-884c-cwd")
        );
        let nested = "/c/Users/role/AppData/Local/Temp/claude-997/cwd-d2_03";
        assert_eq!(
            cwd_file(&wrapped(nested), TEMP).unwrap(),
            format!(r"{TEMP}\claude-997\cwd-d2_03")
        );
        for outside in [
            "/c/Users/role/AppData/Local/Temp",
            "/c/Users/role/AppData/Local/Temp/",
            "/c/Users/role/AppData/Local/Tempx/claude-cwd",
            "/c/Users/role/AppData/Local/Temp/../claude-cwd",
            "/c/Users/role/AppData/Local/Temp/claude-x/cwd-a",
            "/c/Users/role/AppData/Local/Temp/claude-997/cwd-",
            "/c/Users/role/AppData/Local/Temp/claude-997/cwd-a/b",
            "/d/Users/role/AppData/Local/Temp/claude-cwd",
            "/tmp/claude-cwd",
        ] {
            assert_eq!(
                cwd_file(&wrapped(outside), TEMP),
                Err(CWD_OUTSIDE),
                "{outside}"
            );
        }
        let other_temp = r"C:\Users\other\AppData\Local\Temp";
        assert_eq!(cwd_file(&wrapped(CWD), other_temp), Err(CWD_OUTSIDE));
    }

    #[test]
    fn a_temp_that_is_not_a_plain_drive_path_has_no_cwd_file() {
        for temp in [
            r"\\server\share\Temp",
            r"Temp",
            r"C:\a\..\Temp",
            r"CD:\Temp",
        ] {
            assert_eq!(git_bash_path(temp), None, "{temp}");
        }
        assert_eq!(
            git_bash_path(r"C:\Users\A B\Temp\").unwrap(),
            "/c/Users/A B/Temp"
        );
    }

    #[test]
    fn the_candidate_runs_the_command_alone_with_input_closed() {
        assert_eq!(candidate_script("echo hi"), "eval 'echo hi' < /dev/null");
        assert_eq!(
            candidate_script("echo 'a'"),
            r"eval 'echo '\''a'\''' < /dev/null"
        );
    }
}
