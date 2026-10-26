//! Typed accessors on top of the untyped syntax tree.

use std::borrow::Cow;

use text_size::TextRange;

use crate::{SyntaxKind, SyntaxNode, SyntaxToken};

/// A typed view of a syntax node.
pub trait AstNode<'a>: Sized + Copy {
    /// Whether nodes of `kind` can be viewed as `Self`.
    fn can_cast(kind: SyntaxKind) -> bool;
    /// Wraps `node` without checking its kind.
    fn from_unchecked(node: SyntaxNode<'a>) -> Self;
    /// The underlying node.
    fn syntax(&self) -> SyntaxNode<'a>;

    /// Wraps `node` when its kind fits.
    fn cast(node: SyntaxNode<'a>) -> Option<Self> {
        Self::can_cast(node.kind()).then(|| Self::from_unchecked(node))
    }
}

macro_rules! ast_node {
    ($(#[$meta:meta])* $name:ident, $($kind:ident)|+) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name<'a>(SyntaxNode<'a>);

        impl<'a> AstNode<'a> for $name<'a> {
            fn can_cast(kind: SyntaxKind) -> bool {
                matches!(kind, $(SyntaxKind::$kind)|+)
            }
            fn from_unchecked(node: SyntaxNode<'a>) -> Self {
                $name(node)
            }
            fn syntax(&self) -> SyntaxNode<'a> {
                self.0
            }
        }
    };
}

ast_node!(
    /// The document.
    Root, ROOT
);
ast_node!(
    /// `[table]` or `[[array.of.tables]]` with its entries.
    Table, TABLE | ARRAY_TABLE
);
ast_node!(
    /// `[table]` or `[[array.of.tables]]`.
    Header, TABLE_HEADER | ARRAY_TABLE_HEADER
);
ast_node!(
    /// `key = value`
    Entry, ENTRY
);
ast_node!(
    /// `a."b".c`
    Key, KEY
);
ast_node!(
    /// `[1, 2]`
    Array, ARRAY
);
ast_node!(
    /// `{ a = 1 }`
    InlineTable, INLINE_TABLE
);

fn children<'a, N: AstNode<'a>>(node: SyntaxNode<'a>) -> impl Iterator<Item = N> + use<'a, N> {
    node.children().filter_map(N::cast)
}

impl<'a> Root<'a> {
    /// Wraps the root of a tree.
    pub fn from_root(node: SyntaxNode<'a>) -> Root<'a> {
        debug_assert_eq!(node.kind(), SyntaxKind::ROOT);
        Root(node)
    }

    /// The entries before the first table header.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'a>> + use<'a> {
        children(self.0)
    }

    /// The tables, in order.
    pub fn tables(&self) -> impl Iterator<Item = Table<'a>> + use<'a> {
        children(self.0)
    }
}

impl<'a> Table<'a> {
    /// The header.
    pub fn header(&self) -> Option<Header<'a>> {
        children(self.0).next()
    }

    /// Whether this is an `[[array of tables]]` element.
    pub fn is_array(&self) -> bool {
        self.0.kind() == SyntaxKind::ARRAY_TABLE
    }

    /// The entries below the header.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'a>> + use<'a> {
        children(self.0)
    }
}

impl<'a> Header<'a> {
    /// The name of the table.
    pub fn key(&self) -> Option<Key<'a>> {
        children(self.0).next()
    }

    /// Whether this is an `[[array of tables]]` header.
    pub fn is_array(&self) -> bool {
        self.0.kind() == SyntaxKind::ARRAY_TABLE_HEADER
    }
}

impl<'a> Entry<'a> {
    /// The key.
    pub fn key(&self) -> Option<Key<'a>> {
        children(self.0).next()
    }

    /// The `=` token.
    pub fn eq(&self) -> Option<SyntaxToken<'a>> {
        self.0.child_token(SyntaxKind::EQ)
    }

    /// The value.
    pub fn value(&self) -> Option<Value<'a>> {
        self.0.children_with_tokens().skip_while(|e| e.kind() != SyntaxKind::EQ).find_map(|e| {
            match e {
                crate::SyntaxElement::Node(n) => Value::cast_node(n),
                crate::SyntaxElement::Token(t) => Value::cast_token(t),
            }
        })
    }
}

impl<'a> Key<'a> {
    /// The dot separated parts.
    pub fn segments(&self) -> impl Iterator<Item = KeySegment<'a>> + use<'a> {
        self.0
            .child_tokens()
            .filter(|t| !t.kind().is_trivia() && t.kind() != SyntaxKind::DOT)
            .map(KeySegment)
    }

    /// The decoded parts.
    pub fn path(&self) -> Vec<String> {
        self.segments().map(|s| s.value().into_owned()).collect()
    }
}

/// One part of a dotted key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeySegment<'a>(SyntaxToken<'a>);

impl<'a> KeySegment<'a> {
    /// The token.
    pub fn syntax(&self) -> SyntaxToken<'a> {
        self.0
    }

    /// The decoded key.
    pub fn value(&self) -> Cow<'a, str> {
        decode(self.0)
    }

    /// The range of the token.
    pub fn text_range(&self) -> TextRange {
        self.0.text_range()
    }
}

/// A value: a scalar token, an array or an inline table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Value<'a> {
    /// A string, number, boolean or date-time. An `UNKNOWN` token is an
    /// invalid value.
    Scalar(SyntaxToken<'a>),
    /// `[...]`
    Array(Array<'a>),
    /// `{...}`
    InlineTable(InlineTable<'a>),
}

impl<'a> Value<'a> {
    fn cast_node(node: SyntaxNode<'a>) -> Option<Value<'a>> {
        Array::cast(node)
            .map(Value::Array)
            .or_else(|| InlineTable::cast(node).map(Value::InlineTable))
    }

    fn cast_token(token: SyntaxToken<'a>) -> Option<Value<'a>> {
        let kind = token.kind();
        (kind.is_scalar() || kind == SyntaxKind::UNKNOWN || kind == SyntaxKind::BARE_KEY)
            .then_some(Value::Scalar(token))
    }

    /// The range of the value.
    pub fn text_range(&self) -> TextRange {
        match self {
            Value::Scalar(t) => t.text_range(),
            Value::Array(a) => a.0.text_range(),
            Value::InlineTable(t) => t.0.text_range(),
        }
    }
}

impl<'a> Array<'a> {
    /// The items.
    pub fn values(&self) -> impl Iterator<Item = Value<'a>> + use<'a> {
        self.0.children_with_tokens().filter_map(|e| match e {
            crate::SyntaxElement::Node(n) => Value::cast_node(n),
            crate::SyntaxElement::Token(t) => Value::cast_token(t),
        })
    }
}

impl<'a> InlineTable<'a> {
    /// The entries.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'a>> + use<'a> {
        children(self.0)
    }
}

/// The decoded value of a key or string token.
pub fn decode(token: SyntaxToken<'_>) -> Cow<'_, str> {
    match token.kind().to_token() {
        Some(kind) => toml_lexer::unquote(kind, token.text()),
        None => Cow::Borrowed(token.text()),
    }
}
