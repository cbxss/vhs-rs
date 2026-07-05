//! Token types for the VHS-compatible tape language.
//!
//! Ported from vhs/token/token.go, with vhs_rs extensions: `Assert`, `Capture`,
//! and a working `Home` keyword (VHS defines the token but never wires it up).

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
}

/// A lexer token: type, literal text, and source position (1-based line, column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token_type: TokenType,
    pub literal: String,
    pub line: usize,
    pub column: usize,
}

/// Maps keyword strings to token types. Bare words that aren't keywords are strings.
pub fn lookup_identifier(ident: &str) -> TokenType {
    match ident {
        "em" => TokenType::Em,
        "px" => TokenType::Px,
        "ms" => TokenType::Milliseconds,
        "s" => TokenType::Seconds,
        "m" => TokenType::Minutes,
        "Set" => TokenType::Set,
        "Sleep" => TokenType::Sleep,
        "Type" => TokenType::Type,
        "Enter" => TokenType::Enter,
        "Space" => TokenType::Space,
        "Backspace" => TokenType::Backspace,
        "Delete" => TokenType::Delete,
        "Insert" => TokenType::Insert,
        "Ctrl" => TokenType::Ctrl,
        "Alt" => TokenType::Alt,
        "Shift" => TokenType::Shift,
        "Down" => TokenType::Down,
        "Left" => TokenType::Left,
        "Right" => TokenType::Right,
        "Up" => TokenType::Up,
        "PageUp" => TokenType::PageUp,
        "PageDown" => TokenType::PageDown,
        "ScrollUp" => TokenType::ScrollUp,
        "ScrollDown" => TokenType::ScrollDown,
        "Tab" => TokenType::Tab,
        "Escape" => TokenType::Escape,
        "End" => TokenType::End,
        "Home" => TokenType::Home,
        "Hide" => TokenType::Hide,
        "Require" => TokenType::Require,
        "Show" => TokenType::Show,
        "Output" => TokenType::Output,
        "Shell" => TokenType::Shell,
        "FontFamily" => TokenType::FontFamily,
        "MarginFill" => TokenType::MarginFill,
        "Margin" => TokenType::Margin,
        "WindowBar" => TokenType::WindowBar,
        "WindowBarSize" => TokenType::WindowBarSize,
        "BorderRadius" => TokenType::BorderRadius,
        "FontSize" => TokenType::FontSize,
        "Framerate" => TokenType::Framerate,
        "Height" => TokenType::Height,
        "LetterSpacing" => TokenType::LetterSpacing,
        "LineHeight" => TokenType::LineHeight,
        "PlaybackSpeed" => TokenType::PlaybackSpeed,
        "TypingSpeed" => TokenType::TypingSpeed,
        "Padding" => TokenType::Padding,
        "Theme" => TokenType::Theme,
        "Width" => TokenType::Width,
        "LoopOffset" => TokenType::LoopOffset,
        "WaitTimeout" => TokenType::WaitTimeout,
        "WaitPattern" => TokenType::WaitPattern,
        "Wait" => TokenType::Wait,
        "Source" => TokenType::Source,
        "CursorBlink" => TokenType::CursorBlink,
        "true" | "false" => TokenType::Boolean,
        "Screenshot" => TokenType::Screenshot,
        "Copy" => TokenType::Copy,
        "Paste" => TokenType::Paste,
        "Env" => TokenType::Env,
        "Assert" => TokenType::Assert,
        "Capture" => TokenType::Capture,
        _ => TokenType::String,
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
            Em => "em",
            Milliseconds => "ms",
            Minutes => "m",
            Px => "px",
            Seconds => "s",
            Eof => "EOF",
            Illegal => "ILLEGAL",
            Alt => "Alt",
            Backspace => "Backspace",
            Ctrl => "Ctrl",
            Delete => "Delete",
            End => "End",
            Enter => "Enter",
            Escape => "Escape",
            Home => "Home",
            Insert => "Insert",
            PageDown => "PageDown",
            PageUp => "PageUp",
            ScrollDown => "ScrollDown",
            ScrollUp => "ScrollUp",
            Sleep => "Sleep",
            Space => "Space",
            Tab => "Tab",
            Shift => "Shift",
            Comment => "COMMENT",
            Number => "NUMBER",
            String => "STRING",
            Json => "JSON",
            Regex => "REGEX",
            Boolean => "BOOLEAN",
            Down => "Down",
            Left => "Left",
            Right => "Right",
            Up => "Up",
            Hide => "Hide",
            Output => "Output",
            Require => "Require",
            Set => "Set",
            Show => "Show",
            Source => "Source",
            Type => "Type",
            Screenshot => "Screenshot",
            Copy => "Copy",
            Paste => "Paste",
            Shell => "Shell",
            Env => "Env",
            FontFamily => "FontFamily",
            FontSize => "FontSize",
            Framerate => "Framerate",
            PlaybackSpeed => "PlaybackSpeed",
            Height => "Height",
            Width => "Width",
            LetterSpacing => "LetterSpacing",
            LineHeight => "LineHeight",
            TypingSpeed => "TypingSpeed",
            Padding => "Padding",
            Theme => "Theme",
            LoopOffset => "LoopOffset",
            MarginFill => "MarginFill",
            Margin => "Margin",
            WindowBar => "WindowBar",
            WindowBarSize => "WindowBarSize",
            BorderRadius => "BorderRadius",
            Wait => "Wait",
            WaitTimeout => "WaitTimeout",
            WaitPattern => "WaitPattern",
            CursorBlink => "CursorBlink",
            Assert => "Assert",
            Capture => "Capture",
        }
    }
}

impl std::fmt::Display for TokenType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
