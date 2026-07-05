//! Lexer + parser integration tests, ported from VHS's Go test suites
//! (vhs/lexer/lexer_test.go, vhs/parser/parser_test.go), plus vterm-specific
//! tests for the Assert/Capture extensions and the validate() pass.

use vterm::lexer::Lexer;
use vterm::parser::{Parser, validate};
use vterm::token::TokenType;
use vterm::{command::Command, parse_tape};

use TokenType::*;

fn assert_tokens(input: &str, expected: &[(TokenType, &str)]) {
    let mut lexer = Lexer::new(input);
    for (i, (want_type, want_literal)) in expected.iter().enumerate() {
        let tok = lexer.next_token();
        assert_eq!(
            tok.token_type, *want_type,
            "tests[{i}] - tokentype wrong. expected={want_type:?}, got={:?} (literal {:?})",
            tok.token_type, tok.literal
        );
        assert_eq!(
            tok.literal, *want_literal,
            "tests[{i}] - literal wrong. expected={want_literal:?}, got={:?}",
            tok.literal
        );
    }
}

fn assert_commands(cmds: &[Command], expected: &[(TokenType, &str, &str)]) {
    assert_eq!(
        cmds.len(),
        expected.len(),
        "Expected {} commands, got {}; {cmds:?}",
        expected.len(),
        cmds.len()
    );
    for (i, (want_type, want_options, want_args)) in expected.iter().enumerate() {
        assert_eq!(
            cmds[i].command_type, *want_type,
            "Expected command {i} to be {want_type:?}, got {:?}",
            cmds[i].command_type
        );
        assert_eq!(
            cmds[i].args, *want_args,
            "Expected command {i} to have args {want_args:?}, got {:?}",
            cmds[i].args
        );
        assert_eq!(
            cmds[i].options, *want_options,
            "Expected command {i} to have options {want_options:?}, got {:?}",
            cmds[i].options
        );
    }
}

fn fixture_all_tape() -> std::string::String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/all.tape");
    std::fs::read_to_string(path).expect("could not read all.tape file")
}

/// Port of TestNextToken (vhs/lexer/lexer_test.go).
#[test]
fn test_next_token() {
    let input = r#"
Output examples/out.gif
Set FontSize 42
Set Padding 5
Set CursorBlink false
Type "echo 'Hello, world!'"
Enter
Type@.1 "echo 'Hello, world!'"
Left 3
Sleep 1
Right@100ms 3
ScrollUp 3
ScrollDown@100ms 2
Sleep 500ms
Ctrl+C
Enter
Ctrl+@
Ctrl+\
Alt+]
Shift+[
Sleep .1
Sleep 100ms
Sleep 2
Wait+Screen@1m /foobar/
Wait+Screen@1m /foo\/bar/
Wait+Screen@1m /foo\\/
Wait+Screen@1m /foo\\\/bar/"#;

    let expected: &[(TokenType, &str)] = &[
        (Output, "Output"),
        (String, "examples/out.gif"),
        (Set, "Set"),
        (FontSize, "FontSize"),
        (Number, "42"),
        (Set, "Set"),
        (Padding, "Padding"),
        (Number, "5"),
        (Set, "Set"),
        (CursorBlink, "CursorBlink"),
        (Boolean, "false"),
        (Type, "Type"),
        (String, "echo 'Hello, world!'"),
        (Enter, "Enter"),
        (Type, "Type"),
        (At, "@"),
        (Number, ".1"),
        (String, "echo 'Hello, world!'"),
        (Left, "Left"),
        (Number, "3"),
        (Sleep, "Sleep"),
        (Number, "1"),
        (Right, "Right"),
        (At, "@"),
        (Number, "100"),
        (Milliseconds, "ms"),
        (Number, "3"),
        (ScrollUp, "ScrollUp"),
        (Number, "3"),
        (ScrollDown, "ScrollDown"),
        (At, "@"),
        (Number, "100"),
        (Milliseconds, "ms"),
        (Number, "2"),
        (Sleep, "Sleep"),
        (Number, "500"),
        (Milliseconds, "ms"),
        (Ctrl, "Ctrl"),
        (Plus, "+"),
        (String, "C"),
        (Enter, "Enter"),
        (Ctrl, "Ctrl"),
        (Plus, "+"),
        (At, "@"),
        (Ctrl, "Ctrl"),
        (Plus, "+"),
        (Backslash, "\\"),
        (Alt, "Alt"),
        (Plus, "+"),
        (RightBracket, "]"),
        (Shift, "Shift"),
        (Plus, "+"),
        (LeftBracket, "["),
        (Sleep, "Sleep"),
        (Number, ".1"),
        (Sleep, "Sleep"),
        (Number, "100"),
        (Milliseconds, "ms"),
        (Sleep, "Sleep"),
        (Number, "2"),
        (Wait, "Wait"),
        (Plus, "+"),
        (String, "Screen"),
        (At, "@"),
        (Number, "1"),
        (Minutes, "m"),
        (Regex, "foobar"),
        (Wait, "Wait"),
        (Plus, "+"),
        (String, "Screen"),
        (At, "@"),
        (Number, "1"),
        (Minutes, "m"),
        (Regex, r"foo\/bar"),
        (Wait, "Wait"),
        (Plus, "+"),
        (String, "Screen"),
        (At, "@"),
        (Number, "1"),
        (Minutes, "m"),
        (Regex, r"foo\\"),
        (Wait, "Wait"),
        (Plus, "+"),
        (String, "Screen"),
        (At, "@"),
        (Number, "1"),
        (Minutes, "m"),
        (Regex, r"foo\\\/bar"),
    ];

    assert_tokens(input, expected);
}

/// Port of TestLexTapeFile (vhs/lexer/lexer_test.go), against
/// tests/fixtures/all.tape.
#[test]
fn test_lex_tape_file() {
    let input = fixture_all_tape();

    let theme_json = r##"{ "name": "Whimsy", "black": "#535178", "red": "#ef6487", "green": "#5eca89", "yellow": "#fdd877", "blue": "#65aef7", "purple": "#aa7ff0", "cyan": "#43c1be", "white": "#ffffff", "brightBlack": "#535178", "brightRed": "#ef6487", "brightGreen": "#5eca89", "brightYellow": "#fdd877", "brightBlue": "#65aef7", "brightPurple": "#aa7ff0", "brightCyan": "#43c1be", "brightWhite": "#ffffff", "background": "#29283b", "foreground": "#b3b0d6", "selectionBackground": "#3d3c58", "cursorColor": "#b3b0d6" }"##;

    let expected: &[(TokenType, &str)] = &[
        (Comment, " All Commands"),
        (Comment, " Output:"),
        (Output, "Output"),
        (String, "examples/fixtures/all.gif"),
        (Output, "Output"),
        (String, "examples/fixtures/all.mp4"),
        (Output, "Output"),
        (String, "examples/fixtures/all.webm"),
        (Comment, " Settings:"),
        (Set, "Set"),
        (Shell, "Shell"),
        (String, "fish"),
        (Set, "Set"),
        (FontSize, "FontSize"),
        (Number, "22"),
        (Set, "Set"),
        (FontFamily, "FontFamily"),
        (String, "DejaVu Sans Mono"),
        (Set, "Set"),
        (Height, "Height"),
        (Number, "600"),
        (Set, "Set"),
        (Width, "Width"),
        (Number, "1200"),
        (Set, "Set"),
        (LetterSpacing, "LetterSpacing"),
        (Number, "1"),
        (Set, "Set"),
        (LineHeight, "LineHeight"),
        (Number, "1.2"),
        (Set, "Set"),
        (Theme, "Theme"),
        (Json, theme_json),
        (Set, "Set"),
        (Theme, "Theme"),
        (String, "Catppuccin Mocha"),
        (Set, "Set"),
        (Padding, "Padding"),
        (Number, "50"),
        (Set, "Set"),
        (Framerate, "Framerate"),
        (Number, "60"),
        (Set, "Set"),
        (PlaybackSpeed, "PlaybackSpeed"),
        (Number, "2"),
        (Set, "Set"),
        (TypingSpeed, "TypingSpeed"),
        (Number, ".1"),
        (Set, "Set"),
        (LoopOffset, "LoopOffset"),
        (Number, "60.4"),
        (Set, "Set"),
        (LoopOffset, "LoopOffset"),
        (Number, "20.99"),
        (Percent, "%"),
        (Set, "Set"),
        (CursorBlink, "CursorBlink"),
        (Boolean, "false"),
        (Comment, " Sleep:"),
        (Sleep, "Sleep"),
        (Number, "1"),
        (Sleep, "Sleep"),
        (Number, "500"),
        (Milliseconds, "ms"),
        (Sleep, "Sleep"),
        (Number, ".5"),
        (Sleep, "Sleep"),
        (Number, "0.5"),
        (Comment, " Type:"),
        (Type, "Type"),
        (At, "@"),
        (Number, ".5"),
        (String, "All"),
        (Type, "Type"),
        (At, "@"),
        (Number, "500"),
        (Milliseconds, "ms"),
        (String, "All"),
        (Type, "Type"),
        (String, "Double Quote"),
        (Type, "Type"),
        (String, "\"Single\" Quote"),
        (Type, "Type"),
        (String, r#""Backtick" 'Quote'"#),
        (Comment, " Keys:"),
        (Backspace, "Backspace"),
        (Backspace, "Backspace"),
        (Number, "2"),
        (Backspace, "Backspace"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Delete, "Delete"),
        (Delete, "Delete"),
        (Number, "2"),
        (Delete, "Delete"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Insert, "Insert"),
        (Insert, "Insert"),
        (Number, "2"),
        (Insert, "Insert"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Down, "Down"),
        (Down, "Down"),
        (Number, "2"),
        (Down, "Down"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (PageDown, "PageDown"),
        (PageDown, "PageDown"),
        (Number, "2"),
        (PageDown, "PageDown"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (ScrollDown, "ScrollDown"),
        (ScrollDown, "ScrollDown"),
        (Number, "2"),
        (ScrollDown, "ScrollDown"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Enter, "Enter"),
        (Enter, "Enter"),
        (Number, "2"),
        (Enter, "Enter"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Space, "Space"),
        (Space, "Space"),
        (Number, "2"),
        (Space, "Space"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Tab, "Tab"),
        (Tab, "Tab"),
        (Number, "2"),
        (Tab, "Tab"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Left, "Left"),
        (Left, "Left"),
        (Number, "2"),
        (Left, "Left"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Right, "Right"),
        (Right, "Right"),
        (Number, "2"),
        (Right, "Right"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Up, "Up"),
        (Up, "Up"),
        (Number, "2"),
        (Up, "Up"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (PageUp, "PageUp"),
        (PageUp, "PageUp"),
        (Number, "2"),
        (PageUp, "PageUp"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (ScrollUp, "ScrollUp"),
        (ScrollUp, "ScrollUp"),
        (Number, "2"),
        (ScrollUp, "ScrollUp"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Down, "Down"),
        (Down, "Down"),
        (Number, "2"),
        (Down, "Down"),
        (At, "@"),
        (Number, "1"),
        (Number, "3"),
        (Comment, " Control:"),
        (Ctrl, "Ctrl"),
        (Plus, "+"),
        (String, "C"),
        (Ctrl, "Ctrl"),
        (Plus, "+"),
        (String, "L"),
        (Ctrl, "Ctrl"),
        (Plus, "+"),
        (String, "R"),
        (Comment, " Alt:"),
        (Alt, "Alt"),
        (Plus, "+"),
        (String, "."),
        (Alt, "Alt"),
        (Plus, "+"),
        (String, "L"),
        (Alt, "Alt"),
        (Plus, "+"),
        (String, "i"),
        (Comment, " Display:"),
        (Hide, "Hide"),
        (Show, "Show"),
    ];

    assert_tokens(&input, expected);
}

/// Port of TestParser (vhs/parser/parser_test.go).
#[test]
fn test_parser() {
    let input = r#"
Set TypingSpeed 100ms
Set WaitTimeout 1m
Set WaitPattern /foo/
Type "echo 'Hello, World!'"
Enter
Backspace@0.1 5
Backspace@.1 5
Backspace@1 5
Backspace@100ms 5
Delete 2
Insert 2
Right 3
Left 3
Up@50ms
Down 2
ScrollUp 4
ScrollDown@100ms 2
Ctrl+C
Ctrl+L
Alt+.
Sleep 100ms
Sleep 3
Wait
Wait+Screen
Wait@100ms /foobar/"#;

    let expected: &[(TokenType, &str, &str)] = &[
        (Set, "TypingSpeed", "100ms"),
        (Set, "WaitTimeout", "1m"),
        (Set, "WaitPattern", "foo"),
        (Type, "", "echo 'Hello, World!'"),
        (Enter, "", "1"),
        (Backspace, "0.1s", "5"),
        (Backspace, ".1s", "5"),
        (Backspace, "1s", "5"),
        (Backspace, "100ms", "5"),
        (Delete, "", "2"),
        (Insert, "", "2"),
        (Right, "", "3"),
        (Left, "", "3"),
        (Up, "50ms", "1"),
        (Down, "", "2"),
        (ScrollUp, "", "4"),
        (ScrollDown, "100ms", "2"),
        (Ctrl, "", "C"),
        (Ctrl, "", "L"),
        (Alt, "", "."),
        (Sleep, "", "100ms"),
        (Sleep, "", "3s"),
        (Wait, "", "Line"),
        (Wait, "", "Screen"),
        (Wait, "100ms", "Line foobar"),
    ];

    let mut p = Parser::new(input);
    let cmds = p.parse();
    assert!(
        p.errors().is_empty(),
        "expected no parse errors, got: {:?}",
        p.errors()
    );
    assert_commands(&cmds, expected);
}

/// Port of TestParserErrors (vhs/parser/parser_test.go): error Display strings
/// must match VHS's " %2d:%-2d │ %s" format exactly.
#[test]
fn test_parser_errors() {
    let input = "
Type Enter
Type \"echo 'Hello, World!'\" Enter
Foo
Sleep Bar";

    let mut p = Parser::new(input);
    let _ = p.parse();

    let expected = [
        " 2:6  │ Type expects string",
        " 4:1  │ Invalid command: Foo",
        " 5:1  │ Expected time after Sleep",
        " 5:7  │ Invalid command: Bar",
    ];

    let errors = p.errors();
    assert_eq!(
        errors.len(),
        expected.len(),
        "Expected {} errors, got {}: {errors:?}",
        expected.len(),
        errors.len()
    );
    for (i, want) in expected.iter().enumerate() {
        assert_eq!(
            errors[i].to_string(),
            *want,
            "Expected error {i} to be [{want}], got ({})",
            errors[i]
        );
    }
}

/// Port of TestParseTapeFile (vhs/parser/parser_test.go). Note: .mp4/.webm
/// outputs DO parse — vterm rejects them in the separate validate() pass.
#[test]
fn test_parse_tape_file() {
    let input = fixture_all_tape();

    let theme_json = r##"{ "name": "Whimsy", "black": "#535178", "red": "#ef6487", "green": "#5eca89", "yellow": "#fdd877", "blue": "#65aef7", "purple": "#aa7ff0", "cyan": "#43c1be", "white": "#ffffff", "brightBlack": "#535178", "brightRed": "#ef6487", "brightGreen": "#5eca89", "brightYellow": "#fdd877", "brightBlue": "#65aef7", "brightPurple": "#aa7ff0", "brightCyan": "#43c1be", "brightWhite": "#ffffff", "background": "#29283b", "foreground": "#b3b0d6", "selectionBackground": "#3d3c58", "cursorColor": "#b3b0d6" }"##;

    let expected: &[(TokenType, &str, &str)] = &[
        (Output, ".gif", "examples/fixtures/all.gif"),
        (Output, ".mp4", "examples/fixtures/all.mp4"),
        (Output, ".webm", "examples/fixtures/all.webm"),
        (Set, "Shell", "fish"),
        (Set, "FontSize", "22"),
        (Set, "FontFamily", "DejaVu Sans Mono"),
        (Set, "Height", "600"),
        (Set, "Width", "1200"),
        (Set, "LetterSpacing", "1"),
        (Set, "LineHeight", "1.2"),
        (Set, "Theme", theme_json),
        (Set, "Theme", "Catppuccin Mocha"),
        (Set, "Padding", "50"),
        (Set, "Framerate", "60"),
        (Set, "PlaybackSpeed", "2"),
        (Set, "TypingSpeed", ".1s"),
        (Set, "LoopOffset", "60.4%"),
        (Set, "LoopOffset", "20.99%"),
        (Set, "CursorBlink", "false"),
        (Sleep, "", "1s"),
        (Sleep, "", "500ms"),
        (Sleep, "", ".5s"),
        (Sleep, "", "0.5s"),
        (Type, ".5s", "All"),
        (Type, "500ms", "All"),
        (Type, "", "Double Quote"),
        (Type, "", "\"Single\" Quote"),
        (Type, "", r#""Backtick" 'Quote'"#),
        (Backspace, "", "1"),
        (Backspace, "", "2"),
        (Backspace, "1s", "3"),
        (Delete, "", "1"),
        (Delete, "", "2"),
        (Delete, "1s", "3"),
        (Insert, "", "1"),
        (Insert, "", "2"),
        (Insert, "1s", "3"),
        (Down, "", "1"),
        (Down, "", "2"),
        (Down, "1s", "3"),
        (PageDown, "", "1"),
        (PageDown, "", "2"),
        (PageDown, "1s", "3"),
        (ScrollDown, "", "1"),
        (ScrollDown, "", "2"),
        (ScrollDown, "1s", "3"),
        (Enter, "", "1"),
        (Enter, "", "2"),
        (Enter, "1s", "3"),
        (Space, "", "1"),
        (Space, "", "2"),
        (Space, "1s", "3"),
        (Tab, "", "1"),
        (Tab, "", "2"),
        (Tab, "1s", "3"),
        (Left, "", "1"),
        (Left, "", "2"),
        (Left, "1s", "3"),
        (Right, "", "1"),
        (Right, "", "2"),
        (Right, "1s", "3"),
        (Up, "", "1"),
        (Up, "", "2"),
        (Up, "1s", "3"),
        (PageUp, "", "1"),
        (PageUp, "", "2"),
        (PageUp, "1s", "3"),
        (ScrollUp, "", "1"),
        (ScrollUp, "", "2"),
        (ScrollUp, "1s", "3"),
        (Down, "", "1"),
        (Down, "", "2"),
        (Down, "1s", "3"),
        (Ctrl, "", "C"),
        (Ctrl, "", "L"),
        (Ctrl, "", "R"),
        (Alt, "", "."),
        (Alt, "", "L"),
        (Alt, "", "i"),
        (Hide, "", ""),
        (Show, "", ""),
    ];

    let mut p = Parser::new(&input);
    let cmds = p.parse();
    assert!(
        p.errors().is_empty(),
        "expected no parse errors, got: {:?}",
        p.errors()
    );
    assert_commands(&cmds, expected);
}

/// Port of TestParseCtrl (vhs/parser/parser_test.go), via full parses.
#[test]
fn test_parse_ctrl() {
    struct Case {
        name: &'static str,
        tape: &'static str,
        want_args: &'static [&'static str],
        want_err: bool,
    }

    let tests = [
        Case {
            name: "should parse with multiple modifiers",
            tape: "Ctrl+Shift+Alt+C",
            want_args: &["Shift", "Alt", "C"],
            want_err: false,
        },
        Case {
            name: "should not parse with out of order modifiers",
            tape: "Ctrl+Shift+C+Alt",
            want_args: &[],
            want_err: true,
        },
        Case {
            name: "should not parse with out of order modifiers (trailing key)",
            tape: "Ctrl+Shift+C+Alt+C",
            want_args: &[],
            want_err: true,
        },
        Case {
            name: "should parse Ctrl+Left",
            tape: "Ctrl+Left",
            want_args: &["Left"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Right",
            tape: "Ctrl+Right",
            want_args: &["Right"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Up",
            tape: "Ctrl+Up",
            want_args: &["Up"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Down",
            tape: "Ctrl+Down",
            want_args: &["Down"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Alt+Left",
            tape: "Ctrl+Alt+Left",
            want_args: &["Alt", "Left"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Alt+Right",
            tape: "Ctrl+Alt+Right",
            want_args: &["Alt", "Right"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Alt+Up",
            tape: "Ctrl+Alt+Up",
            want_args: &["Alt", "Up"],
            want_err: false,
        },
        Case {
            name: "should parse Ctrl+Alt+Down",
            tape: "Ctrl+Alt+Down",
            want_args: &["Alt", "Down"],
            want_err: false,
        },
        Case {
            name: "Ctrl+Backspace",
            tape: "Ctrl+Backspace",
            want_args: &["Backspace"],
            want_err: false,
        },
        Case {
            name: "Ctrl+Space",
            tape: "Ctrl+Space",
            want_args: &["Space"],
            want_err: false,
        },
    ];

    for tc in &tests {
        let mut p = Parser::new(tc.tape);
        let cmds = p.parse();

        if tc.want_err {
            assert!(
                !p.errors().is_empty(),
                "{}: expected to parse with errors but was success",
                tc.name
            );
            continue;
        }

        assert!(
            p.errors().is_empty(),
            "{}: expected to parse with no errors but got {:?}",
            tc.name,
            p.errors()
        );
        assert_eq!(cmds.len(), 1, "{}: expected one command", tc.name);
        assert_eq!(cmds[0].command_type, Ctrl, "{}", tc.name);
        let args: Vec<&str> = cmds[0].args.split(' ').collect();
        assert_eq!(
            args, tc.want_args,
            "{}: args wrong, expected {:?}, got {:?}",
            tc.name, tc.want_args, args
        );
    }
}

/// RAII temp tape file with a unique absolute path under the OS temp dir,
/// so parallel test runs never collide (the Go tests used cwd-relative
/// "source.tape", which does).
struct TempTape(std::path::PathBuf);

impl TempTape {
    fn new(tag: &str, ext: &str, contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "vterm_test_{}_{}_{tag}.{ext}",
            std::process::id(),
            std::thread::current()
                .name()
                .unwrap_or("t")
                .replace("::", "_"),
        ));
        std::fs::write(&path, contents).expect("write temp tape");
        TempTape(path)
    }

    fn path(&self) -> std::string::String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempTape {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn parse_errors_of(tape: &str) -> Vec<vterm::error::ParseError> {
    let mut p = Parser::new(tape);
    let _ = p.parse();
    p.into_errors()
}

/// Port of TestParseSource (vhs/parser/parser_test.go).
#[test]
fn test_parse_source_ok() {
    let src = TempTape::new("src_ok", "tape", "Type \"echo 'Welcome to VHS!'\"");
    let tape = format!("Source \"{}\"", src.path());

    let mut p = Parser::new(&tape);
    let cmds = p.parse();
    assert!(
        p.errors().is_empty(),
        "expected no errors, got {:?}",
        p.errors()
    );
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command_type, Type);
    assert_eq!(cmds[0].args, "echo 'Welcome to VHS!'");
    assert_eq!(cmds[0].source, src.path());
}

#[test]
fn test_parse_source_not_found() {
    let missing = std::env::temp_dir().join(format!(
        "vterm_test_{}_source_missing.tape",
        std::process::id()
    ));
    let missing = missing.to_string_lossy().into_owned();
    let errors = parse_errors_of(&format!("Source \"{missing}\""));
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, format!("File {missing} not found"));
}

#[test]
fn test_parse_source_wrong_extension() {
    let src = TempTape::new("src_ext", "vhs", "Type \"echo 'Welcome to VHS!'\"");
    let errors = parse_errors_of(&format!("Source \"{}\"", src.path()));
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Expected file with .tape extension");
}

#[test]
fn test_parse_source_missing_path() {
    let errors = parse_errors_of("Source");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Expected path after Source");
}

#[test]
fn test_parse_source_nested() {
    let src = TempTape::new(
        "src_nested",
        "tape",
        "Type \"echo 'Welcome to VHS!'\"\nSource magic.tape\nType \"goodbye\"\n",
    );
    let errors = parse_errors_of(&format!("Source \"{}\"", src.path()));
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Nested Source detected");
}

/// Port of TestParseScreeenshot (vhs/parser/parser_test.go).
#[test]
fn test_parse_screenshot() {
    let errors = parse_errors_of("Screenshot step_one_screenshot.jpg");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Expected file with .png extension");

    let errors = parse_errors_of("Screenshot");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Expected path after Screenshot");
}

// ---------------------------------------------------------------------------
// vterm extensions: Assert, Capture, validate(), Home
// ---------------------------------------------------------------------------

#[test]
fn test_assert_default_scope_is_screen() {
    let mut p = Parser::new("Assert /foo/");
    let cmds = p.parse();
    assert!(p.errors().is_empty(), "unexpected errors: {:?}", p.errors());
    assert_commands(&cmds, &[(Assert, "", "Screen foo")]);
}

#[test]
fn test_assert_line_scope() {
    let mut p = Parser::new("Assert+Line /bar/");
    let cmds = p.parse();
    assert!(p.errors().is_empty(), "unexpected errors: {:?}", p.errors());
    assert_commands(&cmds, &[(Assert, "", "Line bar")]);
}

#[test]
fn test_assert_with_timeout() {
    let mut p = Parser::new("Assert+Line@5s /x/");
    let cmds = p.parse();
    assert!(p.errors().is_empty(), "unexpected errors: {:?}", p.errors());
    assert_commands(&cmds, &[(Assert, "5s", "Line x")]);
}

#[test]
fn test_assert_requires_regex() {
    let errors = parse_errors_of("Assert");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Assert expects /regex/");
}

#[test]
fn test_assert_rejects_bad_regex() {
    let errors = parse_errors_of("Assert /foo(/");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].msg.contains("Invalid regular expression"),
        "unexpected message: {}",
        errors[0].msg
    );
}

#[test]
fn test_capture() {
    let mut p = Parser::new("Capture out.txt");
    let cmds = p.parse();
    assert!(p.errors().is_empty(), "unexpected errors: {:?}", p.errors());
    assert_commands(&cmds, &[(Capture, "", "out.txt")]);

    let errors = parse_errors_of("Capture out.png");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].msg, "Expected file with .txt extension");
}

#[test]
fn test_validate_rejects_video_outputs() {
    let (cmds, errors) = parse_tape("Output x.mp4\nType \"hi\"\n");
    // The parser itself accepts the mp4 output (VHS grammar compatibility)...
    assert_eq!(cmds[0].command_type, Output);
    assert_eq!(cmds[0].options, ".mp4");
    // ...but validation rejects it, mentioning why.
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].msg.contains("ffmpeg"),
        "expected ffmpeg mention, got: {}",
        errors[0].msg
    );
}

#[test]
fn test_validate_rejects_mid_tape_geometry_set() {
    let (_, errors) = parse_tape("Type \"x\"\nSet FontSize 20\n");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .msg
            .contains("Set FontSize cannot appear after commands have started"),
        "unexpected message: {}",
        errors[0].msg
    );
    assert_eq!(errors[0].token.line, 2);
}

#[test]
fn test_validate_allows_mid_tape_runtime_set() {
    let (_, errors) = parse_tape("Type \"x\"\nSet TypingSpeed 10ms\n");
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn test_validate_directly() {
    let mut p = Parser::new("Output frames/\n");
    let cmds = p.parse();
    assert!(p.errors().is_empty());
    let errors = validate(&cmds);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].msg.contains("PNG frame directories"),
        "unexpected message: {}",
        errors[0].msg
    );
}

/// vterm fix over VHS: `Home` is a working keypress (VHS defines the token but
/// never wires it into the parser).
#[test]
fn test_home_keypress() {
    let mut p = Parser::new("Home 2");
    let cmds = p.parse();
    assert!(p.errors().is_empty(), "unexpected errors: {:?}", p.errors());
    assert_commands(&cmds, &[(Home, "", "2")]);
}
