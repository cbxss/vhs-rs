//! Token types for the VHS-compatible tape language.
//!
//! Ported from vhs/token/token.go, with vhs_rs extensions: `Assert`,
//! `Capture`, `Screen`, and a working `Home` keyword (VHS defines the token
//! but never wires it up).

use std::collections::HashMap;
use std::sync::LazyLock;

/// A token's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    At,
    Equal,
    Plus,
    Percent,
    Slash,
    Backslash,
    Dot,
    Minus,
    RightBracket,
    LeftBracket,
    Caret,

    Em,
    Milliseconds,
    Minutes,
    Px,
    Seconds,

    Eof,
    Illegal,

    Alt,
    Backspace,
    Ctrl,
    Delete,
    End,
    Enter,
    Escape,
    Home,
    Insert,
    PageDown,
    PageUp,
    ScrollDown,
    ScrollUp,
    Sleep,
    Space,
    Tab,
    Shift,

    Comment,
    Number,
    String,
    Json,
    Regex,
    Boolean,

    Down,
    Left,
    Right,
    Up,

    Hide,
    Output,
    Require,
    Set,
    Show,
    Source,
    Type,
    Screenshot,
    Copy,
    Paste,
    Shell,
    Env,
    FontFamily,
    FontSize,
    Framerate,
    PlaybackSpeed,
    Height,
    Width,
    LetterSpacing,
    LineHeight,
    TypingSpeed,
    Padding,
    Theme,
    LoopOffset,
    MarginFill,
    Margin,
    WindowBar,
    WindowBarSize,
    BorderRadius,
    Wait,
    WaitTimeout,
    WaitPattern,
    CursorBlink,

    // vhs_rs extensions
    Assert,
    Capture,
    Screen,
}

/// A lexer token: type, literal text, and source position (1-based line, column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token_type: TokenType,
    pub literal: String,
    pub line: usize,
    pub column: usize,
}

/// Single source of truth for the keyword <-> string mapping: every token type
/// whose `as_str` rendering is also its tape-language spelling.
///
/// `lookup_identifier` resolves identifiers through this table (forward), and
/// `TokenType::as_str` renders keyword tokens through it (reverse).
const KEYWORDS: &[(TokenType, &str)] = &[
    (TokenType::Em, "em"),
    (TokenType::Px, "px"),
    (TokenType::Milliseconds, "ms"),
    (TokenType::Seconds, "s"),
    (TokenType::Minutes, "m"),
    (TokenType::Set, "Set"),
    (TokenType::Sleep, "Sleep"),
    (TokenType::Type, "Type"),
    (TokenType::Enter, "Enter"),
    (TokenType::Space, "Space"),
    (TokenType::Backspace, "Backspace"),
    (TokenType::Delete, "Delete"),
    (TokenType::Insert, "Insert"),
    (TokenType::Ctrl, "Ctrl"),
    (TokenType::Alt, "Alt"),
    (TokenType::Shift, "Shift"),
    (TokenType::Down, "Down"),
    (TokenType::Left, "Left"),
    (TokenType::Right, "Right"),
    (TokenType::Up, "Up"),
    (TokenType::PageUp, "PageUp"),
    (TokenType::PageDown, "PageDown"),
    (TokenType::ScrollUp, "ScrollUp"),
    (TokenType::ScrollDown, "ScrollDown"),
    (TokenType::Tab, "Tab"),
    (TokenType::Escape, "Escape"),
    (TokenType::End, "End"),
    (TokenType::Home, "Home"),
    (TokenType::Hide, "Hide"),
    (TokenType::Require, "Require"),
    (TokenType::Show, "Show"),
    (TokenType::Output, "Output"),
    (TokenType::Shell, "Shell"),
    (TokenType::FontFamily, "FontFamily"),
    (TokenType::MarginFill, "MarginFill"),
    (TokenType::Margin, "Margin"),
    (TokenType::WindowBar, "WindowBar"),
    (TokenType::WindowBarSize, "WindowBarSize"),
    (TokenType::BorderRadius, "BorderRadius"),
    (TokenType::FontSize, "FontSize"),
    (TokenType::Framerate, "Framerate"),
    (TokenType::Height, "Height"),
    (TokenType::LetterSpacing, "LetterSpacing"),
    (TokenType::LineHeight, "LineHeight"),
    (TokenType::PlaybackSpeed, "PlaybackSpeed"),
    (TokenType::TypingSpeed, "TypingSpeed"),
    (TokenType::Padding, "Padding"),
    (TokenType::Theme, "Theme"),
    (TokenType::Width, "Width"),
    (TokenType::LoopOffset, "LoopOffset"),
    (TokenType::WaitTimeout, "WaitTimeout"),
    (TokenType::WaitPattern, "WaitPattern"),
    (TokenType::Wait, "Wait"),
    (TokenType::Source, "Source"),
    (TokenType::CursorBlink, "CursorBlink"),
    (TokenType::Screenshot, "Screenshot"),
    (TokenType::Copy, "Copy"),
    (TokenType::Paste, "Paste"),
    (TokenType::Env, "Env"),
    (TokenType::Assert, "Assert"),
    (TokenType::Capture, "Capture"),
    (TokenType::Screen, "Screen"),
];

static KEYWORD_LOOKUP: LazyLock<HashMap<&'static str, TokenType>> =
    LazyLock::new(|| KEYWORDS.iter().map(|&(t, s)| (s, t)).collect());

static KEYWORD_NAMES: LazyLock<HashMap<TokenType, &'static str>> =
    LazyLock::new(|| KEYWORDS.iter().map(|&(t, s)| (t, s)).collect());

/// Maps keyword strings to token types. Bare words that aren't keywords are strings.
pub fn lookup_identifier(ident: &str) -> TokenType {
    match ident {
        "true" | "false" => TokenType::Boolean,
        _ => KEYWORD_LOOKUP
            .get(ident)
            .copied()
            .unwrap_or(TokenType::String),
    }
}

/// Whether a token is a `Set` setting name.
pub fn is_setting(t: TokenType) -> bool {
    use TokenType::*;
    matches!(
        t,
        Shell
            | FontFamily
            | FontSize
            | LetterSpacing
            | LineHeight
            | Framerate
            | TypingSpeed
            | Theme
            | PlaybackSpeed
            | Height
            | Width
            | Padding
            | LoopOffset
            | MarginFill
            | Margin
            | WindowBar
            | WindowBarSize
            | BorderRadius
            | CursorBlink
            | WaitTimeout
            | WaitPattern
    )
}

/// Whether a token is a modifier key.
pub fn is_modifier(t: TokenType) -> bool {
    matches!(t, TokenType::Alt | TokenType::Shift)
}

impl TokenType {
    /// Human-readable name, matching VHS's CamelCase rendering of commands/settings.
    pub fn as_str(&self) -> &'static str {
        use TokenType::*;
        match self {
            // Punctuation and literal-kind tokens are not keywords, so they
            // are named here; everything else renders via the KEYWORDS table.
            At => "@",
            Equal => "=",
            Plus => "+",
            Percent => "%",
            Slash => "/",
            Backslash => "\\",
            Dot => ".",
            Minus => "-",
            RightBracket => "]",
            LeftBracket => "[",
            Caret => "^",
            Eof => "EOF",
            Illegal => "ILLEGAL",
            Comment => "COMMENT",
            Number => "NUMBER",
            String => "STRING",
            Json => "JSON",
            Regex => "REGEX",
            Boolean => "BOOLEAN",
            _ => KEYWORD_NAMES
                .get(self)
                .expect("keyword variant missing from KEYWORDS table"),
        }
    }
}

impl std::fmt::Display for TokenType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
