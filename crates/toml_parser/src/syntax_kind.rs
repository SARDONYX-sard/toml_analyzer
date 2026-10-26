//! The [`SyntaxKind`] enum, shared by tokens and nodes of the syntax tree.

use toml_lexer::TokenKind;

macro_rules! syntax_kinds {
    (
        tokens { $($tok:ident = $lex:ident,)* }
        nodes { $($(#[$meta:meta])* $node:ident,)* }
    ) => {
        /// The kind of a syntax tree element: either a token produced by the lexer or a
        /// node produced by the parser.
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum SyntaxKind {
            $(
                #[doc = concat!("Token produced from [`TokenKind::", stringify!($lex), "`].")]
                $tok,
            )*
            /// Virtual token marking the end of input. Never stored in a tree.
            EOF,
            $(
                #[doc = concat!("Node `", stringify!($node), "`.")]
                $(#[$meta])*
                $node,
            )*
        }

        impl SyntaxKind {
            /// Converts a lexer token kind into the corresponding syntax kind.
            pub fn from_token(kind: TokenKind) -> SyntaxKind {
                match kind {
                    $(TokenKind::$lex => SyntaxKind::$tok,)*
                }
            }

            /// Converts back into the lexer token kind, for tokens.
            pub fn to_token(self) -> Option<TokenKind> {
                match self {
                    $(SyntaxKind::$tok => Some(TokenKind::$lex),)*
                    _ => None,
                }
            }
        }
    };
}

syntax_kinds! {
    tokens {
        WHITESPACE = Whitespace,
        NEWLINE = Newline,
        COMMENT = Comment,

        BARE_KEY = BareKey,

        BASIC_STRING = BasicString,
        MULTILINE_BASIC_STRING = MultilineBasicString,
        LITERAL_STRING = LiteralString,
        MULTILINE_LITERAL_STRING = MultilineLiteralString,

        INTEGER = Integer,
        FLOAT = Float,
        BOOL = Bool,
        OFFSET_DATE_TIME = OffsetDateTime,
        LOCAL_DATE_TIME = LocalDateTime,
        LOCAL_DATE = LocalDate,
        LOCAL_TIME = LocalTime,

        EQ = Eq,
        DOT = Dot,
        COMMA = Comma,
        L_BRACK = LBracket,
        R_BRACK = RBracket,
        L_BRACK2 = DoubleLBracket,
        R_BRACK2 = DoubleRBracket,
        L_BRACE = LBrace,
        R_BRACE = RBrace,

        UNKNOWN = Unknown,
    }
    nodes {
        /// The whole document.
        ROOT,
        /// `[header]` and its entries.
        TABLE,
        /// `[[header]]` and its entries.
        ARRAY_TABLE,
        TABLE_HEADER,
        ARRAY_TABLE_HEADER,
        /// `key = value`
        ENTRY,
        /// `a."b".c`
        KEY,
        /// `[1, 2]`
        ARRAY,
        /// `{ a = 1 }`
        INLINE_TABLE,
        ERROR,
    }
}

impl SyntaxKind {
    /// Whether this kind is a token (leaf) rather than a node.
    pub fn is_token(self) -> bool {
        self < SyntaxKind::EOF
    }

    /// Whitespace and comments: skipped by the parser.
    pub fn is_trivia(self) -> bool {
        matches!(self, SyntaxKind::WHITESPACE | SyntaxKind::COMMENT)
    }

    /// A string token of any flavor.
    pub fn is_string(self) -> bool {
        matches!(
            self,
            SyntaxKind::BASIC_STRING
                | SyntaxKind::MULTILINE_BASIC_STRING
                | SyntaxKind::LITERAL_STRING
                | SyntaxKind::MULTILINE_LITERAL_STRING
        )
    }

    /// A token that is a complete value.
    pub fn is_scalar(self) -> bool {
        self.is_string()
            || matches!(
                self,
                SyntaxKind::INTEGER
                    | SyntaxKind::FLOAT
                    | SyntaxKind::BOOL
                    | SyntaxKind::OFFSET_DATE_TIME
                    | SyntaxKind::LOCAL_DATE_TIME
                    | SyntaxKind::LOCAL_DATE
                    | SyntaxKind::LOCAL_TIME
            )
    }

    /// A date, a time or both.
    pub fn is_date_time(self) -> bool {
        matches!(
            self,
            SyntaxKind::OFFSET_DATE_TIME
                | SyntaxKind::LOCAL_DATE_TIME
                | SyntaxKind::LOCAL_DATE
                | SyntaxKind::LOCAL_TIME
        )
    }

    /// A short description, for "expected ..." messages.
    pub fn describe(self) -> &'static str {
        match self {
            SyntaxKind::EQ => "`=`",
            SyntaxKind::DOT => "`.`",
            SyntaxKind::COMMA => "`,`",
            SyntaxKind::L_BRACK => "`[`",
            SyntaxKind::R_BRACK => "`]`",
            SyntaxKind::L_BRACK2 => "`[[`",
            SyntaxKind::R_BRACK2 => "`]]`",
            SyntaxKind::L_BRACE => "`{`",
            SyntaxKind::R_BRACE => "`}`",
            SyntaxKind::NEWLINE => "a newline",
            SyntaxKind::BARE_KEY | SyntaxKind::KEY => "a key",
            SyntaxKind::EOF => "end of file",
            _ => "a token",
        }
    }
}
