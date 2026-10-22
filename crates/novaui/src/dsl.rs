//! `.nui`: a small declarative language for structure, layout and style (novaui-spec.md, "The `.nui` language").
//!
//! ```text
//! token accent-soft = #3360CDFF;             // #RRGGBB, #AARRGGBB, number, or another token
//! style caption-button { width: 46; height: 32; bg-hover: subtle-secondary; }
//! component TitleBar(title) {
//!   Row #bar { height: 32; Text { text: $title; type: caption; grow: 1; } }
//! }
//! Column #root { TitleBar("Gallery") #tb; Text #t { text: $title; } }
//! ```
//!
//! Elements: `Column`/`Box`, `Row`, `Stack` (children absolute by default), `Text`, `Icon`,
//! `Image`, `Button`, `Spacer`, `Edit`, `Custom`, or a component name. `#id` names an
//! element; inside a component instance with an id the name becomes `instance.id`. `.style`
//! applies a style. `$name` is a binding (set later with `Doc::set`) or, inside a component, a
//! parameter. `on-click: name` sends the action `name`.
//!
//! Layout that depends on the window is written as expressions, next to the tokens:
//!
//! ```text
//! let q = max(0, min(window.w / 1706.667, (window.h - 32) / 928));
//! let s_t = q < 1 ? max(0.85, pow(q, 0.647)) : pow(q, 0.647);
//! latch two { enter: band_w >= 876 * s_t; keep: band_w >= 844 * s_t; }
//! let art_w = tween(two ? col_w : band_w, two, 300ms);   // eases across a flip of `two`
//! Column #face { left: m_x; width: art_w; }
//! Text #title { font-size: 28 * s_t; line: 36 * s_t; }
//! ```
//!
//! `let`s and `latch`es are evaluated in source order whenever the window size or a binding they
//! could read changes (`Doc::update`); a property written as an expression is re-applied only when
//! its value changes. Operators: `?:` `||` `&&` `== != < <= > >=` `+ -` `* /` unary `! -`.
//! Functions: `min max clamp pow exp ln sqrt abs floor ceil round lerp smoothstep smoothstep5
//! sin cos keys spring mix tween`. Numbers may carry `dip`, `ms` or `s`. `window.w`, `window.h` (dips),
//! `window.fullscreen`, `window.maximized`. `-` between names needs spaces (`a - b`: names may
//! contain `-`).

use crate::app::Ui;
use crate::element::Id;
use crate::geom::*;
use crate::style::*;
use crate::theme::{token as builtin, Tok};
use nova_icons::Icon;
/// Maps with a fast, per-process seeded hash: keys are the program's own names, not hostile input.
type HashMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::RandomState>;
type HashSet<K> = std::collections::HashSet<K, foldhash::fast::RandomState>;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Num(f32),
    Pct(f32),
    Str(String),
    Ident(String),
    Color(Color),
    Bind(String),
    /// `@key`: a string looked up by key in the active locale (`Ui::t`) when applied.
    Key(String),
    Bool(bool),
    List(Vec<Val>),
    /// A value computed from lets, latches, the window and bindings.
    Expr(Expr),
}

/// An expression, as a sheet holds it (opaque: its syntax is the parser's).
#[derive(Clone, Debug, PartialEq)]
pub struct Expr(pub(crate) std::rc::Rc<Ex>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
}

impl Ex {
    /// The expressions this one is made of, in order: none for a literal, a name or a binding
    /// (a literal's own expressions are its value's).
    pub(crate) fn children(&self) -> impl Iterator<Item = &Ex> {
        let (fixed, list): ([Option<&Ex>; 3], &[Ex]) = match self {
            Ex::Not(a) | Ex::Neg(a) => ([Some(a), None, None], &[]),
            Ex::Bin(_, a, b) => ([Some(a), Some(b), None], &[]),
            Ex::Tern(c, a, b) => ([Some(c), Some(a), Some(b)], &[]),
            Ex::Call(_, args, _) => ([None; 3], args),
            Ex::Lit(_) | Ex::Name(_) | Ex::Bind(_) => ([None; 3], &[]),
        };
        fixed.into_iter().flatten().chain(list)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ex {
    Lit(Val),
    /// A let, a latch, `window.*`, a token, or a bare word (`center`, `none`).
    Name(String),
    Bind(String),
    Not(Box<Ex>),
    Neg(Box<Ex>),
    Bin(Op, Box<Ex>, Box<Ex>),
    Tern(Box<Ex>, Box<Ex>, Box<Ex>),
    /// Function, arguments, call site (tween keeps state per site).
    Call(String, Vec<Ex>, u32),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Decl {
    Let(String, Ex),
    Latch(String, Ex, Ex),
}

impl From<&str> for Val {
    fn from(s: &str) -> Self {
        Val::Str(s.to_string())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Self {
        Val::Str(s)
    }
}
impl From<f32> for Val {
    fn from(v: f32) -> Self {
        Val::Num(v)
    }
}
impl From<bool> for Val {
    fn from(v: bool) -> Self {
        Val::Bool(v)
    }
}
impl From<Color> for Val {
    fn from(v: Color) -> Self {
        Val::Color(v)
    }
}
impl From<Icon> for Val {
    fn from(v: Icon) -> Self {
        Val::Ident(v.name().to_string())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) kind: String,
    pub(crate) id: Option<String>,
    pub(crate) classes: Vec<String>,
    pub(crate) args: Vec<Val>,
    pub(crate) props: Vec<(String, Val)>,
    pub(crate) children: Vec<Node>,
    pub(crate) line: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Component {
    pub(crate) params: Vec<String>,
    pub(crate) body: Vec<Node>,
}

#[derive(Default, Clone, Debug)]
pub struct Sheet {
    /// `let` and `latch` declarations, in source order.
    pub(crate) decls: Vec<Decl>,
    pub(crate) tokens: HashMap<String, Val>,
    pub(crate) styles: HashMap<String, Vec<(String, Val)>>,
    pub(crate) components: HashMap<String, Component>,
    pub(crate) roots: Vec<Node>,
}

use crate::error::Error;

// ---- lexer ----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum T {
    Ident(String),
    Num(f32, bool),
    Str(String),
    Hash(String),
    Dollar(String),
    At(String),
    Dot(String),
    Sym(char),
}

fn lex(src: &str) -> Result<Vec<(T, usize)>, Error> {
    let cs: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    let ident = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    while i < cs.len() {
        let c = cs[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && cs.get(i + 1) == Some(&'/') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && cs.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                if cs[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            let mut s = String::new();
            i += 1;
            while i < cs.len() && cs[i] != '"' {
                if cs[i] == '\\' && i + 1 < cs.len() {
                    i += 1;
                    s.push(match cs[i] {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                } else {
                    s.push(cs[i]);
                }
                i += 1;
            }
            i += 1;
            out.push((T::Str(s), line));
        } else if c == '#'
            || c == '$'
            || (c == '@' && cs.get(i + 1).is_some_and(|c| c.is_alphanumeric()))
            || (c == '.' && cs.get(i + 1).is_some_and(|c| c.is_alphabetic()))
        {
            i += 1;
            let st = i;
            // A key's dots separate its parts (`@menu.play`); a trailing dot is not the key's.
            let dot = |i: usize| match c {
                '$' => cs[i] == '.',
                '@' => cs[i] == '.' && cs.get(i + 1).is_some_and(|c| ident(*c)),
                _ => false,
            };
            while i < cs.len() && (ident(cs[i]) || dot(i)) {
                i += 1;
            }
            let s: String = cs[st..i].iter().collect();
            out.push((
                match c {
                    '#' => T::Hash(s),
                    '$' => T::Dollar(s),
                    '@' => T::At(s),
                    _ => T::Dot(s),
                },
                line,
            ));
        } else if c.is_ascii_digit()
            || (c == '-' && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '.'))
            || (c == '.' && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let st = i;
            i += 1;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            let s: String = cs[st..i].iter().collect();
            let v: f32 = s.parse().map_err(|_| Error::parse(line, format!("bad number {s}")))?;
            let pct = cs.get(i) == Some(&'%');
            if pct {
                i += 1;
            }
            // Units: dip (the unit of every length), ms / s (durations, in seconds).
            let rest: String = cs[i..(i + 3).min(cs.len())].iter().collect();
            let after = |n: usize| cs.get(i + n).is_none_or(|c| !c.is_alphanumeric() && *c != '_');
            let mut v = v;
            if !pct && rest.starts_with("dip") && after(3) {
                i += 3;
            } else if !pct && rest.starts_with("ms") && after(2) {
                i += 2;
                v /= 1000.0;
            } else if !pct && rest.starts_with('s') && after(1) {
                i += 1;
            }
            out.push((T::Num(v, pct), line));
        } else if ident(c) {
            let st = i;
            while i < cs.len() && ident(cs[i]) {
                i += 1;
            }
            let word: String = cs[st..i].iter().collect();
            // A lone '-' is the minus operator.
            if word == "-" {
                out.push((T::Sym('-'), line));
            } else {
                out.push((T::Ident(word), line));
            }
        } else {
            out.push((T::Sym(c), line));
            i += 1;
        }
    }
    Ok(out)
}

// ---- parser ---------------------------------------------------------------------------------

struct P {
    t: Vec<(T, usize)>,
    i: usize,
    /// Names declared by `let` / `latch` so far: a bare use of one is an expression.
    dynamic: HashSet<String>,
    calls: u32,
    /// Names may carry fields (`a.b.c`): a condition's, not a sheet's.
    dotted: bool,
    /// How deep the parser is in elements and expressions now (`nest`).
    depth: u32,
}

/// How deep elements and expressions may nest. The parser recurses once per level, and a sheet
/// can come from outside (a plugin's fragment): without a cap, deep enough nesting overflows the
/// stack, which aborts the process.
const MAX_DEPTH: u32 = 64;

impl P {
    fn new(t: Vec<(T, usize)>, dotted: bool) -> P {
        P { t, i: 0, dynamic: Default::default(), calls: 0, dotted, depth: 0 }
    }
    /// Runs `f` one level deeper, refusing to go past `MAX_DEPTH`.
    fn nest<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R, Error>) -> Result<R, Error> {
        if self.depth >= MAX_DEPTH {
            return self.err(format!("nested more than {MAX_DEPTH} deep"));
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i).map(|t| &t.0)
    }
    fn line(&self) -> usize {
        self.t.get(self.i.min(self.t.len().saturating_sub(1))).map_or(0, |t| t.1)
    }
    fn err<X>(&self, msg: impl Into<String>) -> Result<X, Error> {
        Err(Error::parse(self.line(), msg))
    }
    fn next(&mut self) -> Option<T> {
        let t = self.t.get(self.i).map(|t| t.0.clone());
        self.i += 1;
        t
    }
    fn sym(&mut self, c: char) -> Result<(), Error> {
        match self.next() {
            Some(T::Sym(x)) if x == c => Ok(()),
            o => {
                self.i -= 1;
                self.err(format!("expected '{c}', found {o:?}"))
            }
        }
    }
    fn is_sym(&self, c: char) -> bool {
        matches!(self.peek(), Some(T::Sym(x)) if *x == c)
    }
    fn ident(&mut self) -> Result<String, Error> {
        match self.next() {
            Some(T::Ident(s)) => Ok(s),
            o => {
                self.i -= 1;
                self.err(format!("expected a name, found {o:?}"))
            }
        }
    }

    /// The value a literal token stands for (a number, percentage, string, key, colour, `true`
    /// or `false`); `None` when the token is not a literal.
    fn literal(&self, t: &T) -> Option<Result<Val, Error>> {
        Some(Ok(match t {
            T::Num(v, false) => Val::Num(*v),
            T::Num(v, true) => Val::Pct(*v),
            T::Str(s) => Val::Str(s.clone()),
            T::At(s) => Val::Key(s.clone()),
            T::Hash(h) => match parse_hex(h) {
                Some(c) => Val::Color(c),
                None => return Some(Err(Error::parse(self.line(), format!("bad colour #{h}")))),
            },
            T::Ident(s) if s == "true" => Val::Bool(true),
            T::Ident(s) if s == "false" => Val::Bool(false),
            _ => return None,
        }))
    }

    fn value1(&mut self) -> Result<Val, Error> {
        let t = self.next();
        if let Some(v) = t.as_ref().and_then(|t| self.literal(t)) {
            return v;
        }
        Ok(match t {
            Some(T::Dollar(s)) => Val::Bind(s),
            Some(T::Ident(s)) => Val::Ident(s),
            o => {
                self.i -= 1;
                return self.err(format!("expected a value, found {o:?}"));
            }
        })
    }

    /// Values up to `;` or `}` (several → a list). Each is an expression; a plain literal stays
    /// a literal.
    fn value(&mut self) -> Result<Val, Error> {
        let mut vs = vec![self.item()?];
        while !self.is_sym(';') && !self.is_sym('}') && self.peek().is_some() {
            if self.is_sym(',') {
                self.i += 1;
                continue;
            }
            vs.push(self.item()?);
        }
        Ok(if vs.len() == 1 { vs.pop().unwrap() } else { Val::List(vs) })
    }

    fn item(&mut self) -> Result<Val, Error> {
        let e = self.ex()?;
        Ok(self.lower(e))
    }

    fn lower(&self, e: Ex) -> Val {
        match e {
            Ex::Lit(v) => v,
            Ex::Bind(b) => Val::Bind(b),
            Ex::Name(n) if !self.dynamic.contains(&n) && !n.starts_with("window.") => Val::Ident(n),
            e => Val::Expr(Expr(std::rc::Rc::new(e))),
        }
    }

    fn sym_at(&self, k: usize, c: char) -> bool {
        matches!(self.t.get(self.i + k).map(|t| &t.0), Some(T::Sym(x)) if *x == c)
    }

    fn ex(&mut self) -> Result<Ex, Error> {
        self.nest(Self::ex_in)
    }

    fn ex_in(&mut self) -> Result<Ex, Error> {
        let c = self.ex_bin(0)?;
        if self.is_sym('?') {
            self.i += 1;
            let a = self.ex()?;
            self.sym(':')?;
            let b = self.ex()?;
            return Ok(Ex::Tern(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    /// The binary operator at the cursor, its precedence level and its token count.
    fn op(&self) -> Option<(Op, u8, usize)> {
        let two = |a, b| self.sym_at(0, a) && self.sym_at(1, b);
        Some(if two('|', '|') {
            (Op::Or, 0, 2)
        } else if two('&', '&') {
            (Op::And, 1, 2)
        } else if two('=', '=') {
            (Op::Eq, 2, 2)
        } else if two('!', '=') {
            (Op::Ne, 2, 2)
        } else if two('<', '=') {
            (Op::Le, 2, 2)
        } else if two('>', '=') {
            (Op::Ge, 2, 2)
        } else if self.sym_at(0, '<') {
            (Op::Lt, 2, 1)
        } else if self.sym_at(0, '>') {
            (Op::Gt, 2, 1)
        } else if self.sym_at(0, '+') {
            (Op::Add, 3, 1)
        } else if self.sym_at(0, '-') {
            (Op::Sub, 3, 1)
        } else if self.sym_at(0, '*') {
            (Op::Mul, 4, 1)
        } else if self.sym_at(0, '/') {
            (Op::Div, 4, 1)
        } else {
            return None;
        })
    }

    fn ex_bin(&mut self, min: u8) -> Result<Ex, Error> {
        let mut l = self.ex_unary()?;
        while let Some((op, prec, n)) = self.op() {
            if prec < min {
                break;
            }
            self.i += n;
            let r = self.ex_bin(prec + 1)?;
            l = Ex::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn ex_unary(&mut self) -> Result<Ex, Error> {
        if self.sym_at(0, '!') && !self.sym_at(1, '=') {
            self.i += 1;
            return Ok(Ex::Not(Box::new(self.nest(Self::ex_unary)?)));
        }
        if self.sym_at(0, '-') {
            self.i += 1;
            return Ok(Ex::Neg(Box::new(self.nest(Self::ex_unary)?)));
        }
        self.ex_primary()
    }

    fn ex_primary(&mut self) -> Result<Ex, Error> {
        let t = self.next();
        if let Some(v) = t.as_ref().and_then(|t| self.literal(t)) {
            return v.map(Ex::Lit);
        }
        match t {
            Some(T::Dollar(s)) => Ok(Ex::Bind(s)),
            Some(T::Sym('(')) => {
                let e = self.ex()?;
                self.sym(')')?;
                Ok(e)
            }
            Some(T::Ident(s)) => {
                if self.is_sym('(') {
                    self.i += 1;
                    let mut args = Vec::new();
                    while !self.is_sym(')') {
                        if self.peek().is_none() {
                            return self.err(format!("unclosed call to {s}"));
                        }
                        args.push(self.ex()?);
                        if self.is_sym(',') {
                            self.i += 1;
                        }
                    }
                    self.i += 1;
                    self.calls += 1;
                    return Ok(Ex::Call(s, args, self.calls));
                }
                // `window.w`: a name with a field. In a condition (`Cond`) any name may carry
                // fields: `doc.layers.extra`.
                if let Some(T::Dot(f)) = self.peek().cloned() {
                    if s == "window" || s == "unit" {
                        self.i += 1;
                        return Ok(Ex::Name(format!("{s}.{f}")));
                    }
                    if self.dotted {
                        let mut name = s;
                        while let Some(T::Dot(f)) = self.peek().cloned() {
                            self.i += 1;
                            name = format!("{name}.{f}");
                        }
                        return Ok(Ex::Name(name));
                    }
                }
                Ok(Ex::Name(s))
            }
            o => {
                self.i -= 1;
                self.err(format!("expected a value, found {o:?}"))
            }
        }
    }

    fn props_and_children(&mut self, props: &mut Vec<(String, Val)>, children: &mut Vec<Node>) -> Result<(), Error> {
        self.sym('{')?;
        while !self.is_sym('}') {
            if self.peek().is_none() {
                return self.err("unexpected end: missing '}'");
            }
            // `name: value;` or a child element.
            let is_prop = matches!(self.peek(), Some(T::Ident(_)))
                && matches!(self.t.get(self.i + 1).map(|t| &t.0), Some(T::Sym(':')));
            if is_prop {
                let k = self.ident()?;
                self.sym(':')?;
                let v = self.value()?;
                if self.is_sym(';') {
                    self.i += 1;
                }
                props.push((k, v));
            } else {
                children.push(self.node()?);
            }
        }
        self.sym('}')
    }

    fn node(&mut self) -> Result<Node, Error> {
        self.nest(Self::node_in)
    }

    fn node_in(&mut self) -> Result<Node, Error> {
        let line = self.line();
        let kind = self.ident()?;
        let mut n = Node {
            kind,
            id: None,
            classes: Vec::new(),
            args: Vec::new(),
            props: Vec::new(),
            children: Vec::new(),
            line,
        };
        if self.is_sym('(') {
            self.i += 1;
            while !self.is_sym(')') {
                n.args.push(self.value1()?);
                if self.is_sym(',') {
                    self.i += 1;
                }
            }
            self.i += 1;
        }
        // Shorthand content: Text "hello", Icon Play.
        match self.peek() {
            Some(T::Str(_)) if n.kind == "Text" => {
                let v = self.value1()?;
                n.props.push(("text".into(), v));
            }
            Some(T::Dollar(_) | T::At(_)) if n.kind == "Text" => {
                let v = self.value1()?;
                n.props.push(("text".into(), v));
            }
            Some(T::Ident(_)) if n.kind == "Icon" => {
                let v = self.value1()?;
                n.props.push(("icon".into(), v));
            }
            _ => {}
        }
        loop {
            match self.peek() {
                Some(T::Hash(_)) => {
                    if let Some(T::Hash(h)) = self.next() {
                        n.id = Some(h);
                    }
                }
                Some(T::Dot(_)) => {
                    if let Some(T::Dot(c)) = self.next() {
                        n.classes.push(c);
                    }
                }
                _ => break,
            }
        }
        if self.is_sym('{') {
            let (mut props, mut children) = (std::mem::take(&mut n.props), Vec::new());
            self.props_and_children(&mut props, &mut children)?;
            n.props = props;
            n.children = children;
        } else if self.is_sym(';') {
            self.i += 1;
        }
        Ok(n)
    }
}

fn parse_hex(h: &str) -> Option<Color> {
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        6 => Some(argb(0xFF00_0000 | v)),
        8 => Some(argb(v)),
        3 => {
            let (r, g, b) = ((v >> 8) & 15, (v >> 4) & 15, v & 15);
            Some(argb(0xFF00_0000 | (r * 17) << 16 | (g * 17) << 8 | (b * 17)))
        }
        _ => None,
    }
}

pub fn parse(src: &str) -> Result<Sheet, Error> {
    parse_tokens(lex(src)?)
}

fn parse_tokens(t: Vec<(T, usize)>) -> Result<Sheet, Error> {
    let mut p = P::new(t, false);
    let mut sheet = Sheet::default();
    while p.peek().is_some() {
        match p.peek() {
            Some(T::Ident(k)) if k == "let" => {
                p.i += 1;
                let name = p.ident()?;
                p.sym('=')?;
                let e = p.ex()?;
                p.sym(';')?;
                p.dynamic.insert(name.clone());
                sheet.decls.push(Decl::Let(name, e));
            }
            Some(T::Ident(k)) if k == "latch" => {
                p.i += 1;
                let name = p.ident()?;
                p.sym('{')?;
                let (mut enter, mut keep) = (None, None);
                while !p.is_sym('}') {
                    let k = p.ident()?;
                    p.sym(':')?;
                    let e = p.ex()?;
                    if p.is_sym(';') {
                        p.i += 1;
                    }
                    match k.as_str() {
                        "enter" => enter = Some(e),
                        "keep" => keep = Some(e),
                        o => return p.err(format!("a latch has enter and keep, not {o}")),
                    }
                }
                p.sym('}')?;
                let (Some(enter), Some(keep)) = (enter, keep) else {
                    return p.err(format!("latch {name} needs enter and keep"));
                };
                p.dynamic.insert(name.clone());
                sheet.decls.push(Decl::Latch(name, enter, keep));
            }
            Some(T::Ident(k)) if k == "token" => {
                p.i += 1;
                let name = p.ident()?;
                p.sym('=')?;
                let v = p.value()?;
                p.sym(';')?;
                sheet.tokens.insert(name, v);
            }
            Some(T::Ident(k)) if k == "style" => {
                p.i += 1;
                let name = p.ident()?;
                let (mut props, mut children) = (Vec::new(), Vec::new());
                p.props_and_children(&mut props, &mut children)?;
                if !children.is_empty() {
                    return p.err("a style has no children");
                }
                sheet.styles.insert(name, props);
            }
            Some(T::Ident(k)) if k == "component" => {
                p.i += 1;
                let name = p.ident()?;
                let mut params = Vec::new();
                if p.is_sym('(') {
                    p.i += 1;
                    while !p.is_sym(')') {
                        params.push(p.ident()?);
                        if p.is_sym(',') {
                            p.i += 1;
                        }
                    }
                    p.i += 1;
                }
                p.sym('{')?;
                let mut body = Vec::new();
                while !p.is_sym('}') {
                    if p.peek().is_none() {
                        return p.err("unexpected end in component");
                    }
                    body.push(p.node()?);
                }
                p.sym('}')?;
                sheet.components.insert(name, Component { params, body });
            }
            _ => {
                let n = p.node()?;
                sheet.roots.push(n);
            }
        }
    }
    expansion(&sheet)?;
    Ok(sheet)
}

/// What one instance of a component makes, nested components expanded.
#[derive(Clone, Copy)]
struct Expanded {
    /// Elements (saturating).
    elements: usize,
    /// Levels of elements, the deepest path.
    depth: u32,
}

/// What one instance of each component makes (`Expanded`). A component that contains itself,
/// directly or through others, is an error, and so is one whose elements nest deeper than
/// `MAX_DEPTH`: building either recurses once per level until the stack overflows.
fn expansion(sheet: &Sheet) -> Result<HashMap<&str, Expanded>, Error> {
    type Done<'s> = HashMap<&'s str, Expanded>;
    /// Its depth is relative; `level` is where its roots stand in the expansion being walked
    /// (1-based), checked on the way down so that the walk itself stays shallow.
    fn comp<'s>(
        sheet: &'s Sheet,
        name: &'s str,
        line: usize,
        level: u32,
        open: &mut Vec<&'s str>,
        done: &mut Done<'s>,
    ) -> Result<Expanded, Error> {
        let e = match done.get(name) {
            Some(&e) => e,
            None => {
                if open.contains(&name) {
                    return Err(Error::parse(line, format!("component {name} contains itself")));
                }
                open.push(name);
                let mut e = Expanded { elements: 0, depth: 0 };
                for b in &sheet.components[name].body {
                    let b = node(sheet, b, level, open, done)?;
                    e.elements = e.elements.saturating_add(b.elements);
                    e.depth = e.depth.max(b.depth);
                }
                open.pop();
                done.insert(name, e);
                e
            }
        };
        too_deep(line, level - 1 + e.depth)?;
        Ok(e)
    }
    fn node<'s>(
        sheet: &'s Sheet,
        n: &'s Node,
        level: u32,
        open: &mut Vec<&'s str>,
        done: &mut Done<'s>,
    ) -> Result<Expanded, Error> {
        too_deep(n.line, level)?;
        let mut e = match sheet.components.get_key_value(&n.kind) {
            Some((name, _)) => comp(sheet, name, n.line, level, open, done)?,
            None => Expanded { elements: 1, depth: 1 },
        };
        // Children of an instance hang under its root, one level down.
        for c in &n.children {
            let c = node(sheet, c, level + 1, open, done)?;
            e.elements = e.elements.saturating_add(c.elements);
            e.depth = e.depth.max(c.depth + 1);
        }
        Ok(e)
    }
    fn too_deep(line: usize, level: u32) -> Result<(), Error> {
        if level > MAX_DEPTH {
            return Err(Error::parse(line, format!("elements nest more than {MAX_DEPTH} deep")));
        }
        Ok(())
    }
    let mut done = Done::default();
    for (name, c) in &sheet.components {
        let line = c.body.first().map_or(0, |b| b.line);
        comp(sheet, name, line, 1, &mut Vec::new(), &mut done)?;
    }
    for n in &sheet.roots {
        node(sheet, n, 1, &mut Vec::new(), &mut done)?;
    }
    Ok(done)
}

// ---- instantiation --------------------------------------------------------------------------

/// A parsed document bound to a UI: named elements, bindings, and the sheet for later
/// instantiation of components (lists).
/// What a doc keeps about the elements it built, per element: their names, the bindings and
/// expressions their properties read, their `@key` texts and their per-unit programs; and the
/// index from a binding to the expressions reading it, dropped whenever those change.
#[derive(Default)]
struct Records {
    ids: HashMap<String, Id>,
    binds: HashMap<String, Vec<(Id, String)>>,
    /// Properties written as expressions, with what they read.
    dyn_props: Vec<DynProp>,
    /// Properties written `@key` in the sheet: (element, property, key).
    keyed: Vec<(Id, String, String)>,
    /// Texts drawn per unit (`u-*`).
    unit_progs: Vec<units::UnitProg>,
    /// Binding name to the `dyn_props` reading it; rebuilt when they change.
    bind_index: Option<HashMap<String, Vec<u32>>>,
}

impl Records {
    /// The records of elements `alive` says are gone are dropped; the tween sites of the gone
    /// expressions are returned.
    fn retain_alive(&mut self, alive: impl Fn(Id) -> bool) -> HashSet<u64> {
        for l in self.binds.values_mut() {
            l.retain(|(id, _)| alive(*id));
        }
        self.ids.retain(|_, id| alive(*id));
        self.keyed.retain(|(id, _, _)| alive(*id));
        let dead = self.dyn_props.iter().filter(|p| !alive(p.id)).map(|p| p.site).collect();
        self.dyn_props.retain(|p| alive(p.id));
        self.unit_progs.retain(|p| alive(p.id));
        self.bind_index = None;
        self.binds.retain(|_, ids| !ids.is_empty());
        dead
    }

    /// A property written as an expression is kept (the index is made again).
    fn push_prop(&mut self, p: DynProp) {
        self.dyn_props.push(p);
        self.bind_index = None;
    }

    /// The expressions, taken to be gone through (`put_props` gives them back).
    fn take_props(&mut self) -> Vec<DynProp> {
        self.bind_index = None;
        std::mem::take(&mut self.dyn_props)
    }

    /// `take_props`' expressions back, ahead of any kept meanwhile.
    fn put_props(&mut self, mut props: Vec<DynProp>) {
        props.append(&mut self.dyn_props);
        self.dyn_props = props;
        self.bind_index = None;
    }
}

pub struct Doc {
    pub(crate) sheet: Sheet,
    /// What it keeps about the elements it built.
    rec: Records,
    values: HashMap<String, Val>,
    /// `update_binds`' list of properties to evaluate (kept, not allocated each turn).
    hit: Vec<u32>,
    lets: HashMap<String, Val>,
    tweens: HashMap<(u64, u32), Tween>,
    tweening: bool,
    /// Bindings set since the last `update` (only the expressions reading them re-evaluate).
    changed_binds: HashSet<String>,
    last_key: Option<(u32, u32, bool, bool)>,
    snap_version: u64,
    now: f64,
    win: Win,
    /// Mounts made so far (each mount's names are keyed by its number).
    mounts: u32,
    /// `tree.removal_version` when this doc last dropped its records of removed elements.
    seen_removals: u64,
    /// `ui.strings.version()` when keyed texts were last looked up.
    strings_version: u64,
    /// A program is stale (`update_units` has work).
    units_pending: bool,
    /// `tree.unit_version` when the programs were last checked.
    unit_version: u64,
    /// Which build made each element, and the watched file (debug builds: live reload).
    #[cfg(any(debug_assertions, test))]
    dev: reload::DocDev,
}

/// The message a watched sheet's change wakes the app with (`Doc::watch`, debug builds): the
/// app's next `Doc::update` applies the change. An app can let it pass like any message it
/// does not know, so long as it updates the doc in that turn.
#[derive(Clone, Copy, Debug)]
pub struct SheetChanged;

/// A mounted fragment: its key (its names' prefix) and its root element.
#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    key: String,
    pub root: Id,
}

/// A fragment taken in by `Doc::adopt`: its components, ready to be built many times.
#[derive(Clone, Debug)]
pub struct Adopted {
    key: String,
}

impl Adopted {
    /// The doc's name of the fragment's component `n`.
    fn name(&self, n: &str) -> String {
        under(&self.key, n)
    }
}

/// The doc's name of an adopted fragment's `n` (a style, component or binding): under the key
/// `Doc::adopt` gave it (`under(key, "")` is the prefix of every such name).
fn under(key: &str, n: &str) -> String {
    format!("{key}/{n}")
}

/// A name taken in from outside (`Doc::adopt`, `Doc::mount`): its key starts with `~`, which a
/// host's own names never do.
fn adopted_name(n: &str) -> bool {
    n.starts_with('~')
}

/// A value with each binding it reads rewritten by `to` (`None`: kept as written); in an
/// expression a binding becomes the binding, the expression or the literal it is given.
fn rebind(v: &Val, to: &impl Fn(&str) -> Option<Val>) -> Val {
    match v {
        Val::Bind(n) => to(n).unwrap_or_else(|| v.clone()),
        Val::List(l) => Val::List(l.iter().map(|x| rebind(x, to)).collect()),
        Val::Expr(Expr(e)) => Val::Expr(Expr(std::rc::Rc::new(rebind_ex(e, to)))),
        o => o.clone(),
    }
}

fn rebind_ex(e: &Ex, to: &impl Fn(&str) -> Option<Val>) -> Ex {
    let b = |x: &Ex| Box::new(rebind_ex(x, to));
    match e {
        Ex::Bind(n) => match to(n) {
            Some(Val::Bind(m)) => Ex::Bind(m),
            Some(Val::Expr(Expr(x))) => (*x).clone(),
            Some(v) => Ex::Lit(v),
            None => e.clone(),
        },
        Ex::Not(a) => Ex::Not(b(a)),
        Ex::Neg(a) => Ex::Neg(b(a)),
        Ex::Bin(o, x, y) => Ex::Bin(*o, b(x), b(y)),
        Ex::Tern(c, x, y) => Ex::Tern(b(c), b(x), b(y)),
        Ex::Call(f, a, s) => Ex::Call(f.clone(), a.iter().map(|x| rebind_ex(x, to)).collect(), *s),
        o => o.clone(),
    }
}

/// The bindings (`$name`) a value reads, in expressions too.
fn val_binds(v: &Val, out: &mut std::collections::BTreeSet<String>) {
    fn ex(e: &Ex, out: &mut std::collections::BTreeSet<String>) {
        match e {
            Ex::Bind(n) => {
                out.insert(n.clone());
            }
            Ex::Lit(v) => val_binds(v, out),
            e => e.children().for_each(|x| ex(x, out)),
        }
    }
    match v {
        Val::Bind(n) => {
            out.insert(n.clone());
        }
        Val::List(l) => l.iter().for_each(|x| val_binds(x, out)),
        Val::Expr(Expr(e)) => ex(e, out),
        _ => {}
    }
}

impl Sheet {
    /// Every value the sheet writes, with its property's name: its styles', and every element's
    /// in its components and roots ("" for a component's argument).
    fn each_value(&self, mut f: impl FnMut(&str, &Val)) {
        fn node(n: &Node, f: &mut impl FnMut(&str, &Val)) {
            n.props.iter().for_each(|(k, v)| f(k, v));
            n.args.iter().for_each(|v| f("", v));
            n.children.iter().for_each(|c| node(c, f));
        }
        for st in self.styles.values() {
            st.iter().for_each(|(k, v)| f(k, v));
        }
        for c in self.components.values() {
            c.body.iter().for_each(|n| node(n, &mut f));
        }
        self.roots.iter().for_each(|n| node(n, &mut f));
    }

    /// The names of its components.
    pub fn component_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.components.keys().map(String::as_str)
    }

    /// It defines component `name`.
    pub fn has_component(&self, name: &str) -> bool {
        self.components.contains_key(name)
    }

    /// The bindings (`$name`) the sheet reads anywhere: properties, arguments, expressions,
    /// styles. Sorted, each once.
    pub fn reads(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        self.each_value(|_, v| val_binds(v, &mut out));
        out.into_iter().collect()
    }

    /// What its presses send: every `on-click` it writes out, in elements and styles. One
    /// written as a binding or an expression is not listed (a fragment cannot write one).
    pub fn actions(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.each_value(|k, v| {
            if let ("on-click", Val::Str(a) | Val::Ident(a)) = (k, v) {
                out.push(a.clone());
            }
        });
        out
    }
}

/// Every binding a component's body names, nested components' and styles' included.
fn comp_binds(sheet: &Sheet, comp: &str) -> std::collections::BTreeSet<String> {
    fn node(sheet: &Sheet, n: &Node, seen: &mut Vec<String>, out: &mut std::collections::BTreeSet<String>) {
        n.props.iter().for_each(|(_, v)| val_binds(v, out));
        n.args.iter().for_each(|v| val_binds(v, out));
        for c in &n.classes {
            if let Some(st) = sheet.styles.get(c) {
                st.iter().for_each(|(_, v)| val_binds(v, out));
            }
        }
        if sheet.components.contains_key(&n.kind) && !seen.contains(&n.kind) {
            seen.push(n.kind.clone());
            for b in &sheet.components[&n.kind].body {
                node(sheet, b, seen, out);
            }
        }
        n.children.iter().for_each(|c| node(sheet, c, seen, out));
    }
    let mut out = std::collections::BTreeSet::new();
    let mut seen = vec![comp.to_string()];
    if let Some(c) = sheet.components.get(comp) {
        for b in &c.body {
            node(sheet, b, &mut seen, &mut out);
        }
    }
    out
}

/// A property written as an expression: re-evaluated only when something it reads changed
/// (a binding, a let or latch, the window) or while a tween runs.
struct DynProp {
    id: Id,
    prop: String,
    expr: Val,
    last: Option<Val>,
    /// Lets, latches and other names it reads (`window.*` included).
    names: Vec<String>,
    binds: Vec<String>,
    tween: bool,
    /// It reads `window.*` itself (a let it reads that does is in `names`).
    window: bool,
    /// `property_site(id, prop)`: its tweens' key.
    site: u64,
}

impl DynProp {
    /// The property is the same one on element `id` now (a reload's copy taken over by a live
    /// element): its tweens' key follows.
    #[cfg(any(debug_assertions, test))]
    fn move_to(&mut self, id: Id) {
        self.id = id;
        self.site = property_site(id, &self.prop);
    }

    fn new(id: Id, prop: &str, expr: Val, last: Option<Val>) -> Self {
        let (mut names, mut binds, mut tween) = (Vec::new(), Vec::new(), false);
        fn walk(e: &Ex, names: &mut Vec<String>, binds: &mut Vec<String>, tween: &mut bool) {
            match e {
                Ex::Lit(v) => walk_val(v, names, binds, tween),
                Ex::Name(n) => names.push(n.clone()),
                Ex::Bind(b) => binds.push(b.clone()),
                Ex::Call(f, ..) => *tween |= f == "tween",
                Ex::Not(_) | Ex::Neg(_) | Ex::Bin(..) | Ex::Tern(..) => {}
            }
            for c in e.children() {
                walk(c, names, binds, tween);
            }
        }
        fn walk_val(v: &Val, names: &mut Vec<String>, binds: &mut Vec<String>, tween: &mut bool) {
            match v {
                Val::Expr(Expr(e)) => walk(e, names, binds, tween),
                Val::Bind(b) => binds.push(b.clone()),
                Val::List(l) => l.iter().for_each(|x| walk_val(x, names, binds, tween)),
                _ => {}
            }
        }
        walk_val(&expr, &mut names, &mut binds, &mut tween);
        names.sort();
        names.dedup();
        binds.sort();
        binds.dedup();
        let window = names.iter().any(|n| n.starts_with("window."));
        DynProp { id, prop: prop.to_string(), expr, last, names, binds, tween, window, site: property_site(id, prop) }
    }
}

#[derive(Clone, Copy, Default)]
struct Win {
    w: f32,
    h: f32,
    fullscreen: bool,
    maximized: bool,
}

#[derive(Clone, Debug)]
struct Tween {
    across: bool,
    from: f32,
    t0: f64,
    out: f32,
    running: bool,
}

fn num(v: &Val, sheet: &Sheet) -> f32 {
    match v {
        Val::Num(n) => *n,
        Val::Pct(n) => *n / 100.0,
        Val::Bool(b) => *b as i32 as f32,
        Val::Str(s) => s.parse().unwrap_or(0.0),
        o => resolve_num(sheet, o).unwrap_or(0.0),
    }
}

/// What a comparison or arithmetic operator gives on two numbers (a division by 0 is 0). The
/// logical and equality operators are their evaluators' own: false here.
fn arith(op: Op, a: f32, b: f32) -> Val {
    match op {
        Op::Lt => Val::Bool(a < b),
        Op::Le => Val::Bool(a <= b),
        Op::Gt => Val::Bool(a > b),
        Op::Ge => Val::Bool(a >= b),
        Op::Add => Val::Num(a + b),
        Op::Sub => Val::Num(a - b),
        Op::Mul => Val::Num(a * b),
        Op::Div => Val::Num(if b == 0.0 { 0.0 } else { a / b }),
        Op::Or | Op::And | Op::Eq | Op::Ne => Val::Bool(false),
    }
}

/// A condition over names an app defines (`item.owner == self && page == "details"`): the
/// expression grammar of `let` (`! && || == != < > <= >= ?:`, literals, names with fields). It
/// reads only what `get` gives it; an unknown name is false. No sheet, window or binding.
#[derive(Clone, Debug)]
pub struct Cond(Ex);

impl Cond {
    pub fn parse(src: &str) -> Result<Cond, Error> {
        let mut p = P::new(lex(src)?, true);
        let e = p.ex()?;
        if p.peek().is_some() {
            return p.err("unexpected text after the condition");
        }
        fn plain(e: &Ex) -> bool {
            !matches!(e, Ex::Bind(_) | Ex::Call(..)) && e.children().all(plain)
        }
        if !plain(&e) {
            return Err(Error::parse(1, "a condition has no bindings or calls"));
        }
        Ok(Cond(e))
    }

    /// The names it reads (an app pushes only these).
    pub fn names(&self) -> Vec<String> {
        fn walk(e: &Ex, out: &mut Vec<String>) {
            match e {
                Ex::Name(n) if !out.contains(n) => out.push(n.clone()),
                // A condition has no calls (`parse`).
                Ex::Call(..) => {}
                e => e.children().for_each(|c| walk(c, out)),
            }
        }
        let mut out = Vec::new();
        walk(&self.0, &mut out);
        out
    }

    pub fn eval(&self, get: &dyn Fn(&str) -> Option<Val>) -> bool {
        fn n(v: &Val) -> f32 {
            match v {
                Val::Num(n) | Val::Pct(n) => *n,
                Val::Bool(b) => *b as i32 as f32,
                Val::Str(s) => s.parse().unwrap_or(0.0),
                _ => 0.0,
            }
        }
        fn ev(e: &Ex, get: &dyn Fn(&str) -> Option<Val>) -> Val {
            match e {
                Ex::Lit(v) => v.clone(),
                Ex::Name(k) => get(k).unwrap_or(Val::Bool(false)),
                Ex::Not(a) => Val::Bool(!truth(&ev(a, get))),
                Ex::Neg(a) => Val::Num(-n(&ev(a, get))),
                Ex::Tern(c, a, b) => {
                    if truth(&ev(c, get)) {
                        ev(a, get)
                    } else {
                        ev(b, get)
                    }
                }
                Ex::Bin(Op::Or, a, b) => Val::Bool(truth(&ev(a, get)) || truth(&ev(b, get))),
                Ex::Bin(Op::And, a, b) => Val::Bool(truth(&ev(a, get)) && truth(&ev(b, get))),
                Ex::Bin(op @ (Op::Eq | Op::Ne), a, b) => {
                    let (a, b) = (ev(a, get), ev(b, get));
                    let eq = match (&a, &b) {
                        (Val::Num(_) | Val::Bool(_), _) | (_, Val::Num(_) | Val::Bool(_)) => n(&a) == n(&b),
                        (Val::Str(x) | Val::Ident(x), Val::Str(y) | Val::Ident(y)) => x == y,
                        _ => a == b,
                    };
                    Val::Bool(eq == (*op == Op::Eq))
                }
                Ex::Bin(op, a, b) => arith(*op, n(&ev(a, get)), n(&ev(b, get))),
                Ex::Bind(_) | Ex::Call(..) => Val::Bool(false),
            }
        }
        truth(&ev(&self.0, get))
    }
}

/// What a fragment from outside the app (a plugin's) may say: the element kinds and properties
/// allowed, and its size. The app sets it; the framework enforces it.
pub struct Profile {
    pub kinds: &'static [&'static str],
    pub props: &'static [&'static str],
    pub max_bytes: usize,
    /// Elements one instance of a component makes, the components it names expanded.
    pub max_nodes: usize,
}

/// Parses a fragment: `component` and `style` only (no `let`, `latch`, `token`, no top-level
/// elements, no `window.*`, no calls), every element and property on the profile's list. A
/// fragment is data; anything outside the list is an error with its line.
/// The calls a fragment may make: arithmetic with no state and no clock.
const PURE_CALLS: &[&str] = &[
    "min",
    "max",
    "clamp",
    "pow",
    "exp",
    "ln",
    "sqrt",
    "abs",
    "floor",
    "ceil",
    "round",
    "lerp",
    "smoothstep",
    "smoothstep5",
    "sin",
    "cos",
    "spring",
    "mix",
    "keys",
];

/// A sheet from outside the app (a plugin's), held to `prof`. `keys`: the namespace its
/// `@key` texts resolve in (`@title` reads `ns:title`, the owner's table added with
/// `Strings::add_under` or `replace_under`); `None`: the fragment has no string keys.
pub fn parse_fragment(src: &str, prof: &Profile, keys: Option<&str>) -> Result<Sheet, Error> {
    if src.len() > prof.max_bytes {
        return Err(Error::parse(0, format!("a fragment has at most {} bytes", prof.max_bytes)));
    }
    let mut tokens = lex(src)?;
    if let Some(ns) = keys {
        for (t, _) in &mut tokens {
            if let T::At(k) = t {
                *k = format!("{ns}:{k}");
            }
        }
    }
    let sheet = parse_tokens(tokens)?;
    let keyed = keys.is_some();
    let first = |line: usize, msg: String| Err(Error::parse(line, msg));
    if !sheet.decls.is_empty() {
        return first(0, "a fragment has no let or latch".into());
    }
    if !sheet.tokens.is_empty() {
        return first(0, "a fragment has no token".into());
    }
    if let Some(n) = sheet.roots.first() {
        return first(n.line, "a fragment has only components and styles".into());
    }
    fn val_ok(v: &Val, keyed: bool) -> Result<(), String> {
        fn ex_ok(e: &Ex, keyed: bool) -> Result<(), String> {
            match e {
                Ex::Name(n) if n.starts_with("window.") => Err("a fragment does not read the window".into()),
                // Pure arithmetic only: `tween` keeps state across frames and reads the clock.
                Ex::Call(f, ..) if !PURE_CALLS.contains(&f.as_str()) => Err(format!("a fragment has no call {f}")),
                Ex::Lit(Val::Key(k)) if !keyed => Err(format!("a fragment has no string key @{k}")),
                e => e.children().try_for_each(|a| ex_ok(a, keyed)),
            }
        }
        match v {
            Val::Expr(Expr(e)) => ex_ok(e, keyed),
            // The app's strings are not a fragment's to read: its keys live in its namespace.
            Val::Key(k) if !keyed => Err(format!("a fragment has no string key @{k}")),
            Val::List(l) => l.iter().try_for_each(|v| val_ok(v, keyed)),
            _ => Ok(()),
        }
    }
    // What a press sends is written out: a binding or an expression could name any action.
    fn prop_ok(k: &str, v: &Val, keyed: bool) -> Result<(), String> {
        if k == "on-click" && !matches!(v, Val::Ident(_) | Val::Str(_)) {
            return Err("a fragment's on-click is a name, written out".into());
        }
        val_ok(v, keyed)
    }
    fn walk(n: &Node, prof: &Profile, comps: &HashMap<String, Component>, keyed: bool) -> Result<(), Error> {
        if !prof.kinds.contains(&n.kind.as_str()) && !comps.contains_key(&n.kind) {
            return Err(Error::parse(n.line, format!("element {} is not allowed here", n.kind)));
        }
        for (k, v) in &n.props {
            if !prof.props.contains(&k.as_str()) {
                return Err(Error::parse(n.line, format!("property {k} is not allowed here")));
            }
            prop_ok(k, v, keyed).map_err(|msg| Error::parse(n.line, msg))?;
        }
        n.children.iter().try_for_each(|c| walk(c, prof, comps, keyed))
    }
    for (name, st) in &sheet.styles {
        for (k, v) in st {
            if !prof.props.contains(&k.as_str()) {
                return first(0, format!("property {k} is not allowed here (style {name})"));
            }
            prop_ok(k, v, keyed).map_err(|msg| Error::parse(0, msg))?;
        }
    }
    for c in sheet.components.values() {
        for n in &c.body {
            walk(n, prof, &sheet.components, keyed)?;
        }
    }
    // What an instance makes, not what the source writes: components nest and repeat.
    for (name, e) in expansion(&sheet)? {
        if e.elements > prof.max_nodes {
            return first(0, format!("a fragment has at most {} elements ({name} makes more)", prof.max_nodes));
        }
    }
    Ok(sheet)
}

fn truth(v: &Val) -> bool {
    match v {
        Val::Bool(b) => *b,
        Val::Num(n) | Val::Pct(n) => *n != 0.0,
        Val::Str(s) => !s.is_empty(),
        Val::Ident(s) => s != "none",
        _ => true,
    }
}

/// The smoothstep curve over `t` in 0..=1 (`smoothstep(t)` and `keys`' easing).
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn smootherstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

struct Eval<'a> {
    sheet: &'a Sheet,
    lets: &'a HashMap<String, Val>,
    values: &'a HashMap<String, Val>,
    tweens: &'a mut HashMap<(u64, u32), Tween>,
    win: Win,
    now: f64,
    site: u64,
    tweening: bool,
    /// `unit.*` while a `u-*` expression is evaluated for one unit.
    unit: Option<units::UnitVars>,
}

impl Eval<'_> {
    /// A property's value: an expression evaluated, a list's expressions each; anything else
    /// as written.
    fn val(&mut self, v: &Val) -> Val {
        match v {
            Val::Expr(Expr(e)) => self.eval(e),
            Val::List(l) => Val::List(
                l.iter()
                    .map(|x| match x {
                        Val::Expr(Expr(e)) => self.eval(e),
                        o => o.clone(),
                    })
                    .collect(),
            ),
            o => o.clone(),
        }
    }

    fn name(&self, n: &str) -> Val {
        match n {
            "window.w" => return Val::Num(self.win.w),
            "window.h" => return Val::Num(self.win.h),
            "window.fullscreen" => return Val::Bool(self.win.fullscreen),
            "window.maximized" => return Val::Bool(self.win.maximized),
            _ => {}
        }
        if let (Some(u), Some(f)) = (&self.unit, n.strip_prefix("unit.")) {
            return Val::Num(u.get(f).unwrap_or(0.0));
        }
        if let Some(v) = self.lets.get(n) {
            return v.clone();
        }
        let id = Val::Ident(n.to_string());
        if let Some(x) = resolve_num(self.sheet, &id) {
            return Val::Num(x);
        }
        if n != "none" {
            if let Some(c) = resolve_color(self.sheet, &id) {
                return Val::Color(c);
            }
        }
        id
    }

    fn n(&mut self, e: &Ex) -> f32 {
        let v = self.eval(e);
        num(&v, self.sheet)
    }

    fn eval(&mut self, e: &Ex) -> Val {
        match e {
            Ex::Lit(v) => v.clone(),
            Ex::Name(n) => self.name(n),
            Ex::Bind(b) => self.values.get(b).cloned().unwrap_or(Val::Num(0.0)),
            Ex::Not(a) => {
                let v = self.eval(a);
                Val::Bool(!truth(&v))
            }
            Ex::Neg(a) => Val::Num(-self.n(a)),
            Ex::Tern(c, a, b) => {
                let c = self.eval(c);
                if truth(&c) {
                    self.eval(a)
                } else {
                    self.eval(b)
                }
            }
            Ex::Bin(op, a, b) => match op {
                Op::Or => {
                    let a = self.eval(a);
                    if truth(&a) {
                        Val::Bool(true)
                    } else {
                        let b = self.eval(b);
                        Val::Bool(truth(&b))
                    }
                }
                Op::And => {
                    let a = self.eval(a);
                    if !truth(&a) {
                        Val::Bool(false)
                    } else {
                        let b = self.eval(b);
                        Val::Bool(truth(&b))
                    }
                }
                Op::Eq | Op::Ne => {
                    let (a, b) = (self.eval(a), self.eval(b));
                    let eq = match (&a, &b) {
                        (Val::Num(_) | Val::Bool(_), _) | (_, Val::Num(_) | Val::Bool(_)) => {
                            num(&a, self.sheet) == num(&b, self.sheet)
                        }
                        _ => a == b,
                    };
                    Val::Bool(eq == (*op == Op::Eq))
                }
                _ => {
                    let (a, b) = (self.n(a), self.n(b));
                    arith(*op, a, b)
                }
            },
            Ex::Call(f, args, site) => self.call(f, args, *site),
        }
    }

    fn call(&mut self, f: &str, args: &[Ex], site: u32) -> Val {
        if f == "tween" && self.unit.is_some() {
            // Per unit there is no state to ease from: a unit's look is a function of time.
            return Val::Num(args.first().map_or(0.0, |a| self.n(a)));
        }
        if f == "mix" {
            // mix(a, b, t): colours or numbers.
            let (a, b) = (args.first().map(|a| self.eval(a)), args.get(1).map(|b| self.eval(b)));
            let t = args.get(2).map_or(0.0, |t| self.n(t)).clamp(0.0, 1.0);
            let (ca, cb) = (
                a.as_ref().and_then(|a| resolve_color(self.sheet, a)),
                b.as_ref().and_then(|b| resolve_color(self.sheet, b)),
            );
            if let (Some(ca), Some(cb), false) = (ca, cb, matches!(a, Some(Val::Num(_)))) {
                return Val::Color(std::array::from_fn(|i| ca[i] + (cb[i] - ca[i]) * t));
            }
            let (a, b) = (a.map_or(0.0, |a| num(&a, self.sheet)), b.map_or(0.0, |b| num(&b, self.sheet)));
            return Val::Num(a + (b - a) * t);
        }
        if f == "tween" {
            // tween(value, across, duration): the value, eased from where it was over
            // `duration` (smootherstep) each time `across` flips; follows it exactly otherwise.
            let target = args.first().map_or(0.0, |a| self.n(a));
            let across = args.get(1).map(|a| {
                let v = self.eval(a);
                truth(&v)
            });
            let dur = args.get(2).map_or(0.3, |a| self.n(a)) as f64;
            let key = (self.site, site);
            let now = self.now;
            let t = self.tweens.entry(key).or_insert(Tween {
                across: across.unwrap_or(false),
                from: target,
                t0: now - 10.0,
                out: target,
                running: false,
            });
            if across.is_some_and(|a| a != t.across) {
                t.across = across.unwrap();
                t.from = t.out;
                t.t0 = now;
                t.running = true;
            }
            let k = if dur > 0.0 { ((now - t.t0) / dur) as f32 } else { 1.0 };
            if k >= 1.0 {
                t.running = false;
                t.out = target;
            } else {
                t.out = t.from + (target - t.from) * smootherstep(k);
                self.tweening = true;
            }
            return Val::Num(t.out);
        }
        let v: Vec<f32> = args.iter().map(|a| self.n(a)).collect();
        let a = |i: usize| v.get(i).copied().unwrap_or(0.0);
        Val::Num(match f {
            "min" => v.iter().copied().fold(f32::INFINITY, f32::min),
            "max" => v.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            "clamp" => a(0).max(a(1)).min(a(2).max(a(1))),
            "pow" => a(0).max(0.0).powf(a(1)),
            "exp" => a(0).exp(),
            "ln" => a(0).max(1e-30).ln(),
            "sqrt" => a(0).max(0.0).sqrt(),
            "abs" => a(0).abs(),
            "floor" => a(0).floor(),
            "ceil" => a(0).ceil(),
            "round" => a(0).round(),
            "lerp" => a(0) + (a(1) - a(0)) * a(2),
            "smoothstep" => smoothstep(a(0).clamp(0.0, 1.0)),
            "smoothstep5" => smootherstep(a(0)),
            // keys(x, at0, v0, at1, v1, …): keyframes on x (stops in order): the first value
            // before the first stop, the last after the last, eased (smoothstep) between.
            "keys" => {
                let stops = &v[1.min(v.len())..];
                let pairs = stops.len() / 2;
                let x = a(0);
                let stop = |i: usize| (stops[2 * i], stops[2 * i + 1]);
                if pairs == 0 {
                    0.0
                } else if x <= stop(0).0 {
                    stop(0).1
                } else {
                    let k = (1..pairs).find(|&i| x < stop(i).0);
                    match k {
                        None => stop(pairs - 1).1,
                        Some(i) => {
                            let ((x0, v0), (x1, v1)) = (stop(i - 1), stop(i));
                            let t = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 1.0 };
                            v0 + (v1 - v0) * smoothstep(t)
                        }
                    }
                }
            }
            "sin" => a(0).sin(),
            "cos" => a(0).cos(),
            // spring(t, frequency, damping): a damped spring's step from 0 to 1 at time t (0
            // before 0): damping 1 settles without overshoot, less overshoots and rings.
            "spring" => {
                let (t, f, z) = (a(0), a(1).max(0.0), a(2).clamp(0.0, 1.0));
                if t <= 0.0 {
                    0.0
                } else {
                    let w = std::f32::consts::TAU * f;
                    let decay = (-z * w * t).exp();
                    if z >= 1.0 {
                        1.0 - decay * (1.0 + w * t)
                    } else {
                        let wd = w * (1.0 - z * z).sqrt();
                        1.0 - decay * ((wd * t).cos() + z * w / wd * (wd * t).sin())
                    }
                }
            }
            _ => 0.0,
        })
    }
}

#[cfg(test)]
mod lifecycle_tests;
#[cfg(any(debug_assertions, test))]
mod reload;
mod units;

fn site_of(name: &str) -> u64 {
    use std::hash::BuildHasher;
    foldhash::fast::FixedState::default().hash_one(name)
}

fn property_site(id: Id, property: &str) -> u64 {
    use std::hash::BuildHasher;
    foldhash::fast::FixedState::default().hash_one(("property", id, property))
}

fn has_expr(v: &Val) -> bool {
    match v {
        Val::Expr(_) => true,
        Val::List(l) => l.iter().any(has_expr),
        _ => false,
    }
}

pub fn icon_by_name(s: &str) -> Option<Icon> {
    Icon::ALL.iter().copied().find(|i| i.name() == s)
}

impl Doc {
    /// Entry counts of the growable tables (leak checks): ids, bindings, dynamic props, tweens.
    pub fn sizes(&self) -> [usize; 4] {
        [self.rec.ids.len(), self.rec.binds.values().map(Vec::len).sum(), self.rec.dyn_props.len(), self.tweens.len()]
    }
    pub fn new(sheet: Sheet) -> Self {
        Doc {
            sheet,
            rec: Records::default(),
            values: HashMap::default(),
            hit: Vec::new(),
            lets: HashMap::default(),
            tweens: HashMap::default(),
            tweening: false,
            changed_binds: Default::default(),
            last_key: None,
            snap_version: 0,
            now: 0.0,
            win: Win::default(),
            mounts: 0,
            seen_removals: 0,
            strings_version: 0,
            units_pending: false,
            unit_version: 0,
            #[cfg(any(debug_assertions, test))]
            dev: Default::default(),
        }
    }

    /// Debug builds watch the file (`reload.rs`); a release build does nothing.
    #[cfg(not(any(debug_assertions, test)))]
    pub fn watch(&mut self, _: &mut Ui, _: impl AsRef<std::path::Path>) {}

    /// The locale changed (or a table was added): every text given by key is looked up again,
    /// whether written `@key` in the sheet, set as a binding's value or computed by an expression.
    fn present_keys(&mut self, ui: &mut Ui) {
        self.rec.keyed.retain(|(id, _, _)| ui.tree.alive(*id));
        for (id, prop, key) in &self.rec.keyed {
            apply(ui, &self.sheet, *id, prop, &Val::Key(key.clone()));
        }
        for (name, list) in &self.rec.binds {
            if let Some(v @ Val::Key(_)) = self.values.get(name) {
                for (id, prop) in list {
                    if ui.tree.alive(*id) {
                        apply(ui, &self.sheet, *id, prop, v);
                    }
                }
            }
        }
        for p in &self.rec.dyn_props {
            if let (Some(v @ Val::Key(_)), true) = (&p.last, ui.tree.alive(p.id)) {
                apply(ui, &self.sheet, p.id, &p.prop, v);
            }
        }
    }

    /// The value of a `let` or latch as last evaluated (0 / false when unknown).
    pub fn num(&self, name: &str) -> f32 {
        self.lets.get(name).map_or(0.0, |v| num(v, &self.sheet))
    }

    pub fn flag(&self, name: &str) -> bool {
        self.lets.get(name).is_some_and(truth)
    }

    /// An evaluator over the doc's state: its sheet, lets, bindings, tweens, window and clock;
    /// `site` keys its tweens, `unit` is one text unit's `unit.*`.
    fn evaluator(&mut self, site: u64, unit: Option<units::UnitVars>) -> Eval<'_> {
        Eval {
            sheet: &self.sheet,
            lets: &self.lets,
            values: &self.values,
            tweens: &mut self.tweens,
            win: self.win,
            now: self.now,
            site,
            tweening: false,
            unit,
        }
    }

    fn eval_val(&mut self, v: &Val, site: u64) -> Val {
        let mut ev = self.evaluator(site, None);
        let out = ev.val(v);
        let tweening = ev.tweening;
        self.tweening |= tweening;
        out
    }

    fn eval_ex(&mut self, e: &Ex, site: u64) -> Val {
        let mut ev = self.evaluator(site, None);
        let out = ev.eval(e);
        let tweening = ev.tweening;
        self.tweening |= tweening;
        out
    }

    /// A property's new value is applied when it moved, and kept as the last applied.
    fn show(ui: &mut Ui, sheet: &Sheet, p: &mut DynProp, v: Val) {
        if p.last.as_ref() != Some(&v) {
            apply(ui, sheet, p.id, &p.prop, &v);
            p.last = Some(v);
        }
    }

    /// Evaluates the lets and latches in order; the names whose value moved go to `moved`.
    fn eval_decls(&mut self, mut moved: Option<&mut HashSet<String>>) {
        let decls = std::mem::take(&mut self.sheet.decls);
        for d in &decls {
            let (n, v) = match d {
                Decl::Let(n, e) => (n, self.eval_ex(e, site_of(n))),
                Decl::Latch(n, enter, keep) => {
                    let on = self.lets.get(n).is_some_and(truth);
                    let e = if on { keep } else { enter };
                    let v = self.eval_ex(e, site_of(n));
                    (n, Val::Bool(truth(&v)))
                }
            };
            if self.lets.get(n) != Some(&v) {
                if let Some(m) = moved.as_deref_mut() {
                    m.insert(n.clone());
                }
                self.lets.insert(n.clone(), v);
            }
        }
        self.sheet.decls = decls;
    }

    /// Every expression and unit program is evaluated afresh at the next update, no tween
    /// carried over (a new layout epoch, a reload).
    fn evaluate_all_again(&mut self) {
        self.last_key = None;
        self.tweens.clear();
        self.tweening = false;
        self.units_pending = true;
    }

    fn set_window(&mut self, ui: &Ui) {
        if self.snap_version != ui.snap_version {
            self.snap_version = ui.snap_version;
            self.evaluate_all_again();
        }
        let s = ui.scale();
        self.win = Win {
            w: ui.tree.window_size().0 as f32 / s,
            h: ui.tree.window_size().1 as f32 / s,
            fullscreen: ui.window.fullscreen,
            maximized: ui.window.maximized,
        };
        self.now = ui.now;
    }

    /// Re-evaluates the lets, latches and expression properties when the window or a binding
    /// changed (or a tween is running). Returns true while a tween needs further frames.
    ///
    /// Only what reads a change is evaluated again: a property whose bindings, lets or latches
    /// did not change keeps its value (a window change or a running tween re-evaluates what
    /// depends on them). Lets and latches are few and are evaluated together.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    fn update_binds(&mut self, ui: &mut Ui, changed_binds: &HashSet<String>) {
        let index = self.rec.bind_index.get_or_insert_with(|| {
            let mut ix: HashMap<String, Vec<u32>> = HashMap::default();
            for (i, p) in self.rec.dyn_props.iter().enumerate() {
                for b in &p.binds {
                    let v = ix.entry(b.clone()).or_default();
                    if v.last() != Some(&(i as u32)) {
                        v.push(i as u32);
                    }
                }
            }
            ix
        });
        let mut hit = std::mem::take(&mut self.hit);
        hit.clear();
        hit.extend(changed_binds.iter().filter_map(|b| index.get(b)).flatten().copied());
        hit.sort_unstable();
        hit.dedup();
        for &i in &hit {
            let p = &self.rec.dyn_props[i as usize];
            if !ui.tree.alive(p.id) {
                continue;
            }
            let (expr, site) = (p.expr.clone(), p.site);
            let v = self.eval_val(&expr, site);
            Self::show(ui, &self.sheet, &mut self.rec.dyn_props[i as usize], v);
        }
        self.hit = hit;
    }

    /// Applies what changed since the last update (bindings, the window, running tweens).
    /// Returns whether tweens still run: the owner then updates again every frame (its ticker),
    /// the doc keeps no wake of its own.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn update(&mut self, ui: &mut Ui) -> bool {
        #[cfg(any(debug_assertions, test))]
        self.reload_if_changed(ui);
        // Elements the tree took away since (a hidden flyout's rows): their records go.
        if self.seen_removals != ui.tree.removal_version {
            self.prune(ui);
        }
        if self.strings_version != ui.strings.version() {
            self.strings_version = ui.strings.version();
            self.present_keys(ui);
        }
        self.set_window(ui);
        let key = (self.win.w.to_bits(), self.win.h.to_bits(), self.win.fullscreen, self.win.maximized);
        let window = self.last_key != Some(key);
        // The first update, and the first after a new layout epoch (tweens dropped), evaluates
        // every expression; a window change only those reading the window.
        let all = self.last_key.is_none();
        if !window && self.changed_binds.is_empty() && !self.tweening {
            self.update_units(ui);
            return false;
        }
        let tweening = std::mem::replace(&mut self.tweening, false);
        self.last_key = Some(key);
        let changed_binds = std::mem::take(&mut self.changed_binds);
        // The lets and latches whose value moved (a window change re-evaluates everything).
        let mut moved = HashSet::default();
        self.eval_decls(Some(&mut moved));
        if !window && !tweening && moved.is_empty() {
            // Only bindings moved (a karaoke line's light every frame): visit just the
            // expressions that read them, not every expression of the doc.
            self.update_binds(ui, &changed_binds);
            self.units_touched(&changed_binds, &moved, false);
            self.update_units(ui);
            return self.tweening;
        }
        let mut props = self.rec.take_props();
        props.retain(|p| ui.tree.alive(p.id));
        for p in props.iter_mut() {
            let reads = all
                || (window && p.window)
                || (tweening && p.tween)
                || p.binds.iter().any(|b| changed_binds.contains(b))
                || p.names.iter().any(|n| moved.contains(n));
            if !reads {
                continue;
            }
            let v = self.eval_val(&p.expr, p.site);
            Self::show(ui, &self.sheet, p, v);
        }
        // Expressions registered while applying (none today) are kept.
        self.rec.put_props(props);
        self.units_touched(&changed_binds, &moved, window);
        self.update_units(ui);
        self.tweening
    }

    /// Builds the sheet's root elements under `parent`.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn build(&mut self, ui: &mut Ui, parent: Id) -> Result<(), Error> {
        #[cfg(any(debug_assertions, test))]
        {
            let prev = self.dev.begin(reload::RecKind::Build, parent);
            let res = self.build_roots(ui, parent);
            self.dev.end(prev);
            res
        }
        #[cfg(not(any(debug_assertions, test)))]
        self.build_roots(ui, parent)
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    // A release build has only this body: the same code as before the debug records.
    #[cfg_attr(not(any(debug_assertions, test)), inline(always))]
    fn build_roots(&mut self, ui: &mut Ui, parent: Id) -> Result<(), Error> {
        self.set_window(ui);
        self.eval_decls(None);
        let roots = self.sheet.roots.clone();
        for n in &roots {
            self.inst(ui, parent, n, &HashMap::default(), None)?;
        }
        Ok(())
    }

    /// Instantiates component `name` under `parent` (at `at`, or last). Ids inside it are
    /// prefixed with `prefix.`; returns the component's first root element.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn instantiate(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        at: Option<usize>,
        name: &str,
        args: &[Val],
        prefix: &str,
    ) -> Result<Id, Error> {
        // A fragment's component (`mount`) is not the sheet's: no reload record.
        #[cfg(any(debug_assertions, test))]
        if !adopted_name(name) {
            let kind =
                reload::RecKind::Instance { name: name.to_string(), args: args.to_vec(), prefix: prefix.to_string() };
            let prev = self.dev.begin(kind, parent);
            let res = self.instantiate_body(ui, parent, at, name, args, prefix);
            self.dev.end(prev);
            return res;
        }
        self.instantiate_body(ui, parent, at, name, args, prefix)
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    // A release build has only this body: the same code as before the debug records.
    #[cfg_attr(not(any(debug_assertions, test)), inline(always))]
    fn instantiate_body(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        at: Option<usize>,
        name: &str,
        args: &[Val],
        prefix: &str,
    ) -> Result<Id, Error> {
        let comp = self.sheet.components.get(name).cloned().ok_or(Error::parse(0, format!("no component {name}")))?;
        let env: HashMap<String, Val> = comp.params.iter().cloned().zip(args.iter().cloned()).collect();
        self.inst_body(ui, parent, at, &comp.body, &env, Some(prefix))?
            .ok_or(Error::parse(0, format!("component {name} is empty")))
    }

    /// Builds a component's body under `parent` (from `at`, or last), each root in order;
    /// returns the first root (the instance), none for an empty body.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    fn inst_body(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        at: Option<usize>,
        body: &[Node],
        env: &HashMap<String, Val>,
        prefix: Option<&str>,
    ) -> Result<Option<Id>, Error> {
        let mut first = None;
        for (k, n) in body.iter().enumerate() {
            let id = self.inst_at(ui, parent, at.map(|a| a + k), n, env, prefix)?;
            first.get_or_insert(id);
        }
        Ok(first)
    }

    pub fn id(&self, name: &str) -> Id {
        *self.rec.ids.get(name).unwrap_or_else(|| panic!("no element #{name}"))
    }

    /// Mounts a component of a fragment from outside the app (`parse_fragment`) under `parent`.
    /// Its element ids, bindings and actions are its own: ids under the mount's prefix; `$name`
    /// reads only this mount's values (`set_in`); `on-click: x` reaches the app as
    /// `action_prefix + x`. Its styles are its own, then the host styles named in `base` (the
    /// app's buttons' look, shared on purpose), then the theme's tokens. Where it stands is the
    /// parent's: a fragment's profile has no position.
    pub fn mount(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        frag: &Sheet,
        component: &str,
        base: &[&str],
        action_prefix: &str,
    ) -> Result<Mount, Error> {
        if !frag.components.contains_key(component) {
            return Err(Error::parse(0, format!("no component {component}")));
        }
        let a = self.adopt(frag, base, action_prefix);
        let root = self.instantiate(ui, parent, None, &a.name(component), &[], &a.key)?;
        Ok(Mount { key: a.key, root })
    }

    /// Takes in a fragment's components and styles once, under names of their own (the rules
    /// of `mount`), so that `instantiate_scoped` can build them many times. Each instance
    /// reads its own bindings: what a long list of styled rows (transcript lines) needs, without a
    /// mount (and a copy of the fragment) per row. Forget it with `unmount`'s rules: `drop_adopted`.
    pub fn adopt(&mut self, frag: &Sheet, base: &[&str], action_prefix: &str) -> Adopted {
        self.mounts += 1;
        let key = format!("~m{}", self.mounts);
        // The fragment's names never meet the host's: its styles and components are renamed
        // under the mount's key, its bindings too.
        let rename = |n: &str| under(&key, n);
        let mut styles: HashMap<String, Vec<(String, Val)>> = HashMap::default();
        for b in base {
            if let Some(st) = self.sheet.styles.get(*b) {
                styles.insert(b.to_string(), st.clone());
            }
        }
        for (n, st) in &frag.styles {
            styles.insert(n.clone(), st.clone());
        }
        fn fix_val(v: &Val, key: &str) -> Val {
            rebind(v, &|n| Some(Val::Bind(under(key, n))))
        }
        // A property as the mount has it: its bindings renamed, and what a press sends prefixed,
        // wherever it is written (a node or a style): the fragment's presses reach the app
        // only under its prefix.
        fn fix_prop(k: &str, v: &Val, key: &str, prefix: &str) -> (String, Val) {
            let v = fix_val(v, key);
            if k == "on-click" {
                if let Val::Ident(s) | Val::Str(s) = &v {
                    return (k.to_string(), Val::Str(format!("{prefix}{s}")));
                }
            }
            (k.to_string(), v)
        }
        fn fix(n: &Node, key: &str, prefix: &str, comps: &HashMap<String, Component>) -> Node {
            let kind = if comps.contains_key(&n.kind) { under(key, &n.kind) } else { n.kind.clone() };
            let props = n.props.iter().map(|(k, v)| fix_prop(k, v, key, prefix)).collect();
            // Every class is the mount's own: one it neither defines nor was offered names no
            // style at all (the host's other styles stay out of reach).
            let classes = n.classes.iter().map(|c| under(key, c)).collect();
            Node {
                kind,
                id: n.id.clone(),
                classes,
                args: n.args.iter().map(|a| fix_val(a, key)).collect(),
                props,
                children: n.children.iter().map(|c| fix(c, key, prefix, comps)).collect(),
                line: n.line,
            }
        }
        for (n, st) in &styles {
            let st = st.iter().map(|(k, v)| fix_prop(k, v, &key, action_prefix)).collect();
            self.sheet.styles.insert(rename(n), st);
        }
        for (n, c) in &frag.components {
            let body = c.body.iter().map(|b| fix(b, &key, action_prefix, &frag.components)).collect();
            // `$param` in the body was renamed with every other binding: the parameter is too,
            // so an argument still reaches it.
            let params = c.params.iter().map(|p| rename(p)).collect();
            self.sheet.components.insert(rename(n), Component { params, body });
        }
        Adopted { key }
    }

    /// Builds component `component` of an adopted fragment under `parent` (at `at`, or last).
    /// Its ids are under `scope.`; its `$name` bindings are the instance's own, set with
    /// `set("{scope}/{name}")` (`Adopted::bind`). Returns the component's first root.
    #[allow(clippy::too_many_arguments)]
    pub fn instantiate_scoped(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        at: Option<usize>,
        a: &Adopted,
        component: &str,
        args: &[Val],
        scope: &str,
    ) -> Result<Id, Error> {
        let name = a.name(component);
        let comp =
            self.sheet.components.get(&name).cloned().ok_or(Error::parse(0, format!("no component {component}")))?;
        let mut env: HashMap<String, Val> = comp.params.iter().cloned().zip(args.iter().cloned()).collect();
        // Every binding the fragment names (renamed under its key by `adopt`) maps to the
        // instance's own name. Nested components inherit these (`inst_at`).
        let pre = under(&a.key, "");
        for n in comp_binds(&self.sheet, &name) {
            if comp.params.contains(&n) {
                continue;
            }
            if let Some(tail) = n.strip_prefix(&pre) {
                env.insert(n.clone(), Val::Bind(format!("{scope}/{tail}")));
            }
        }
        self.inst_body(ui, parent, at, &comp.body, &env, Some(scope))?
            .ok_or(Error::parse(0, format!("component {component} is empty")))
    }

    /// Forgets the values of every binding under `scope/` (instances removed for good).
    pub fn forget_scope(&mut self, scope: &str) {
        let pre = format!("{scope}/");
        self.values.retain(|k, _| !k.starts_with(&pre));
        self.changed_binds.retain(|k| !k.starts_with(&pre));
    }

    /// Forgets an adopted fragment's names (its instances are the caller's to remove).
    pub fn drop_adopted(&mut self, a: &Adopted) {
        self.forget_key(&a.key);
    }

    /// Forgets the styles, components and binding values taken in under `key` (`adopt`).
    fn forget_key(&mut self, key: &str) {
        let pre = under(key, "");
        self.sheet.styles.retain(|k, _| !k.starts_with(&pre));
        self.sheet.components.retain(|k, _| !k.starts_with(&pre));
        self.values.retain(|k, _| !k.starts_with(&pre));
    }

    /// Removes a mount's elements and forgets its names.
    pub fn unmount(&mut self, ui: &mut Ui, m: &Mount) {
        ui.tree.remove(m.root);
        self.forget_key(&m.key);
        self.prune(ui);
    }

    /// Sets a mount's own binding `name`.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_in(&mut self, ui: &mut Ui, m: &Mount, name: &str, v: impl Into<Val>) {
        self.set(ui, &under(&m.key, name), v);
    }

    /// An element of a mount by its id in the fragment.
    pub fn get_in(&self, m: &Mount, id: &str) -> Option<Id> {
        self.rec.ids.get(&format!("{}.{id}", m.key)).copied()
    }

    pub fn get(&self, name: &str) -> Option<Id> {
        self.rec.ids.get(name).copied()
    }

    /// Every named element: its full name (`instance.id`) and its id.
    pub fn names(&self) -> impl Iterator<Item = (&str, &Id)> + '_ {
        self.rec.ids.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Sets binding `name` and updates every element that reads it.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set(&mut self, ui: &mut Ui, name: &str, v: impl Into<Val>) {
        let v = v.into();
        if self.values.get(name) == Some(&v) {
            return;
        }
        // A binding set again keeps its key: no allocation for a value that changes per frame.
        match self.values.get_mut(name) {
            Some(slot) => *slot = v.clone(),
            None => {
                self.values.insert(name.to_string(), v.clone());
            }
        }
        if !self.changed_binds.contains(name) {
            self.changed_binds.insert(name.to_string());
        }
        if let Some(list) = self.rec.binds.get_mut(name) {
            list.retain(|(id, _)| ui.tree.alive(*id));
            for (id, prop) in list.iter() {
                apply(ui, &self.sheet, *id, prop, &v);
            }
        }
    }

    /// Empties `id` (its children removed) and forgets their names at once: what a container
    /// built from the sheet needs before it is built again with the same names.
    pub fn clear(&mut self, ui: &mut Ui, id: Id) {
        ui.tree.clear_children(id);
        self.prune(ui);
    }

    /// Forgets bindings of elements that no longer exist. `update` does it when elements were
    /// removed since; call it to forget them sooner (names reused right after a removal).
    pub fn prune(&mut self, ui: &Ui) {
        self.seen_removals = ui.tree.removal_version;
        #[cfg(any(debug_assertions, test))]
        self.dev.prune(ui);
        // Prune on removal, not only when window/bindings next change. Scrolling can create
        // thousands of short-lived component instances without invalidating that update key.
        let dead = self.rec.retain_alive(|id| ui.tree.alive(id));
        self.tweens.retain(|(site, _), _| !dead.contains(site));
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    fn inst(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        n: &Node,
        env: &HashMap<String, Val>,
        prefix: Option<&str>,
    ) -> Result<Id, Error> {
        self.inst_at(ui, parent, None, n, env, prefix)
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    fn inst_at(
        &mut self,
        ui: &mut Ui,
        parent: Id,
        at: Option<usize>,
        n: &Node,
        env: &HashMap<String, Val>,
        prefix: Option<&str>,
    ) -> Result<Id, Error> {
        let full_id = n.id.as_ref().map(|i| match prefix {
            Some(p) if !p.is_empty() => format!("{p}.{i}"),
            _ => i.clone(),
        });
        // A component instance.
        if let Some(comp) = self.sheet.components.get(&n.kind).cloned() {
            let args: Vec<Val> = n.args.iter().map(|a| subst(a, env)).collect();
            let mut cenv: HashMap<String, Val> = comp.params.iter().cloned().zip(args).collect();
            // An adopted fragment's instance bindings (`instantiate_scoped`) reach its nested
            // components; a host's own names never start with `~`.
            for (k, v) in env {
                if adopted_name(k) {
                    cenv.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            for (k, v) in &n.props {
                // `name: value` on an instance passes a parameter by name…
                if comp.params.contains(k) {
                    cenv.insert(k.clone(), subst(v, env));
                }
            }
            let child_prefix = full_id.clone().or(prefix.map(str::to_string));
            let first = self
                .inst_body(ui, parent, at, &comp.body, &cenv, child_prefix.as_deref())?
                .ok_or(Error::parse(n.line, format!("component {} is empty", n.kind)))?;
            #[cfg(any(debug_assertions, test))]
            {
                let body = ui.tree.dev_label(first).unwrap_or("").to_string();
                ui.tree.set_dev_label(first, format!("{} = {body}", reload::label_key(n)));
            }
            if let Some(fid) = full_id {
                self.rec.ids.insert(fid, first);
            }
            // …other props style the component's root, and children are appended to it.
            for (k, v) in &n.props {
                if !comp.params.contains(k) {
                    self.prop(ui, first, k, &subst(v, env), n.line)?;
                }
            }
            for c in &n.children {
                self.inst(ui, first, c, env, prefix)?;
            }
            return Ok(first);
        }
        let (layout, visual, content) =
            kind_defaults(&n.kind).ok_or(Error::parse(n.line, format!("unknown element {}", n.kind)))?;
        let id = match at {
            Some(a) => ui.tree.insert(parent, a, layout, visual, content),
            None => ui.tree.add(parent, layout, visual, content),
        };
        if let Some(fid) = &full_id {
            self.rec.ids.insert(fid.clone(), id);
        }
        #[cfg(any(debug_assertions, test))]
        {
            self.dev.made(id);
            ui.tree.set_dev_label(id, reload::label(n));
        }
        // A mount's elements do not take the host's per-kind styles (a host `style Text`):
        // only what the mount was offered.
        let mounted = prefix.is_some_and(adopted_name);
        if mounted {
        } else if let Some(st) = self.sheet.styles.get(&n.kind).cloned() {
            for (k, v) in &st {
                self.prop(ui, id, k, &subst(v, env), n.line)?;
            }
        }
        for c in &n.classes {
            let st = self.sheet.styles.get(c).cloned().ok_or(Error::parse(n.line, format!("no style .{c}")))?;
            for (k, v) in &st {
                self.prop(ui, id, k, &subst(v, env), n.line)?;
            }
        }
        for (k, v) in &n.props {
            self.prop(ui, id, k, &subst(v, env), n.line)?;
        }
        if n.kind == "Edit" {
            let hint = n.props.iter().find(|(k, _)| k == "hint").and_then(|(_, v)| hint_text(ui, &subst(v, env)));
            // Its text's `size` / `line` / `weight`, written on it (default 14 / 20), are its text's:
            // the field itself holds no text.
            let tc = match ui.tree.el(id).content().clone() {
                Content::Text(t) => {
                    ui.tree.set_content(id, Content::None);
                    TextContent { text: String::new(), ..t }
                }
                _ => TextContent { size: 14.0, line: 20.0, ..Default::default() },
            };
            let fg = ui.tree.el(id).visual().fg;
            let hint_fg = resolve_color(&self.sheet, &Val::Ident("text-secondary".into())).unwrap_or(fg);
            ui.make_edit(id, tc, fg, hint_fg, hint.as_deref().unwrap_or(""));
            let selects =
                n.props.iter().any(|(k, v)| k == "select-on-focus" && matches!(subst(v, env), Val::Bool(true)));
            ui.set_edit_select_on_focus(id, selects);
            let action = n.props.iter().find(|(k, _)| k == "on-edit").and_then(|(_, v)| match subst(v, env) {
                Val::Str(s) | Val::Ident(s) => Some(s),
                _ => None,
            });
            ui.set_edit_action(id, action.as_deref());
        }
        for c in &n.children {
            self.inst(ui, id, c, env, prefix)?;
        }
        Ok(id)
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    fn prop(&mut self, ui: &mut Ui, id: Id, k: &str, v: &Val, line: usize) -> Result<(), Error> {
        if units::PROPS.contains(&k) {
            self.unit_prop(id, k, v);
            return Ok(());
        }
        if let Val::Bind(name) = v {
            self.rec.binds.entry(name.clone()).or_default().push((id, k.to_string()));
            if let Some(cur) = self.values.get(name).cloned() {
                apply(ui, &self.sheet, id, k, &cur);
            }
            return Ok(());
        }
        if FIELD_PROPS.contains(&k) {
            // Set up with the field (`inst`); a key's text follows the locale.
            if let ("hint", Val::Key(key)) = (k, v) {
                self.rec.keyed.push((id, k.to_string(), key.clone()));
            }
            return Ok(());
        }
        if let Val::Key(key) = v {
            if !apply(ui, &self.sheet, id, k, v) {
                return Err(Error::parse(line, format!("bad property {k}: @{key}")));
            }
            self.rec.keyed.push((id, k.to_string(), key.clone()));
            return Ok(());
        }
        // A token whose parts are expressions (a type ramp at the scaling law:
        // `token body = 14 * s_t, 20 * s_t, 400;`) is those expressions where it is used.
        let expanded = match v {
            Val::Ident(t) => token(&self.sheet, t).filter(|x| has_expr(x)).cloned(),
            _ => None,
        };
        let v = expanded.as_ref().unwrap_or(v);
        if has_expr(v) {
            let site = property_site(id, k);
            let now = self.eval_val(v, site);
            self.rec.push_prop(DynProp::new(id, k, v.clone(), Some(now.clone())));
            if !apply(ui, &self.sheet, id, k, &now) {
                return Err(Error::parse(line, format!("bad property {k}: {now:?}")));
            }
            return Ok(());
        }
        if !apply(ui, &self.sheet, id, k, v) {
            return Err(Error::parse(line, format!("bad property {k}: {v:?}")));
        }
        Ok(())
    }
}

/// A value as a component instance has it: each `$param` it reads replaced by its argument.
fn subst(v: &Val, env: &HashMap<String, Val>) -> Val {
    if env.is_empty() {
        return v.clone();
    }
    rebind(v, &|n| env.get(n).cloned())
}

/// A text field's own properties: set up with the field (`inst_at`), not applied one by one.
const FIELD_PROPS: [&str; 3] = ["hint", "select-on-focus", "on-edit"];

/// A text field's hint as shown: written out, or a key's text in the locale.
fn hint_text(ui: &Ui, v: &Val) -> Option<String> {
    match v {
        Val::Str(s) => Some(s.clone()),
        Val::Key(k) => Some(ui.t(k, &[])),
        _ => None,
    }
}

fn kind_defaults(kind: &str) -> Option<(Layout, Visual, Content)> {
    let l = Layout::default();
    let v = Visual::default();
    Some(match kind {
        "Column" | "Box" => (l, v, Content::None),
        "Row" => {
            (Layout { direction: FlexDirection::Row, align_items: Some(AlignItems::Center), ..l }, v, Content::None)
        }
        "Stack" => (l, v, Content::None),
        "Spacer" => (Layout { grow: 1.0, ..l }, v, Content::None),
        "Text" => (Layout { shrink: 1.0, min_width: Len::Px(0.0), ..l }, v, Content::Text(TextContent::default())),
        "Icon" => (l, v, Content::Icon(IconContent { icon: Icon::ALL[0], size: 16.0, cut: None, named: false })),
        // `Path { view: 24; d: "…"; icon-size: 18; }`: a vector mark (vector.rs).
        "Path" => (
            l,
            v,
            Content::Path(crate::style::PathContent {
                shape: crate::vector::Shape::empty(),
                size: 16.0,
                d: None,
                view: 24.0,
            }),
        ),
        "Image" => (l, v, Content::None),
        "Custom" => (l, v, Content::None),
        "Button" => (
            Layout {
                direction: FlexDirection::Row,
                align_items: Some(AlignItems::Center),
                justify: Some(JustifyContent::Center),
                ..l
            },
            Visual { cursor: Cursor::Hand, hit: true, ..v },
            Content::None,
        ),
        "Edit" => {
            (Layout { direction: FlexDirection::Row, align_items: Some(AlignItems::Center), ..l }, v, Content::None)
        }
        _ => return None,
    })
}

fn token<'a>(sheet: &'a Sheet, name: &str) -> Option<&'a Val> {
    sheet.tokens.get(name)
}

/// What a name stands for as a `T`: the sheet's token of that name, resolved again by
/// `again`; else the theme's token, read by `from_theme`.
fn named<T>(
    sheet: &Sheet,
    s: &str,
    again: fn(&Sheet, &Val) -> Option<T>,
    from_theme: impl FnOnce(Tok) -> Option<T>,
) -> Option<T> {
    match token(sheet, s) {
        Some(t) => again(sheet, &t.clone()),
        None => from_theme(builtin(s)?),
    }
}

pub fn resolve_color(sheet: &Sheet, v: &Val) -> Option<Color> {
    match v {
        Val::Color(c) => Some(*c),
        Val::Ident(s) if s == "none" => Some(TRANSPARENT),
        Val::Ident(s) => named(sheet, s, resolve_color, |t| match t {
            Tok::Color(c) => Some(c),
            _ => None,
        }),
        // `color alpha`: a colour at a share of its alpha.
        Val::List(l) if l.len() == 2 => {
            let c = resolve_color(sheet, &l[0])?;
            let k = resolve_num(sheet, &l[1])?;
            Some(with_alpha(c, k))
        }
        _ => None,
    }
}

pub fn resolve_num(sheet: &Sheet, v: &Val) -> Option<f32> {
    match v {
        Val::Num(n) => Some(*n),
        Val::Ident(s) => named(sheet, s, resolve_num, |t| match t {
            Tok::Num(n) => Some(n),
            _ => None,
        }),
        _ => None,
    }
}

fn resolve_len(sheet: &Sheet, v: &Val) -> Option<Len> {
    match v {
        Val::Pct(p) => Some(Len::Pct(*p)),
        Val::Ident(s) if s == "auto" => Some(Len::Auto),
        o => resolve_num(sheet, o).map(Len::Px),
    }
}

fn resolve_type(sheet: &Sheet, v: &Val) -> Option<(f32, f32, u16)> {
    match v {
        Val::Ident(s) => named(sheet, s, resolve_type, |t| match t {
            Tok::Type(a, b, c) => Some((a, b, c)),
            _ => None,
        }),
        Val::List(l) if l.len() == 3 => {
            Some((resolve_num(sheet, &l[0])?, resolve_num(sheet, &l[1])?, resolve_num(sheet, &l[2])? as u16))
        }
        _ => None,
    }
}

fn resolve_shadow(sheet: &Sheet, v: &Val) -> Option<Option<Shadow>> {
    match v {
        Val::Ident(s) if s == "none" => Some(None),
        Val::Ident(s) => named(sheet, s, resolve_shadow, |t| match t {
            Tok::Shadow(sigma, dy, color) => Some(Some(Shadow { sigma, dy, color })),
            _ => None,
        }),
        Val::List(l) if l.len() == 3 => Some(Some(Shadow {
            sigma: resolve_num(sheet, &l[0])?,
            dy: resolve_num(sheet, &l[1])?,
            color: resolve_color(sheet, &l[2])?,
        })),
        _ => None,
    }
}

fn truthy(v: &Val) -> Option<bool> {
    match v {
        Val::Bool(b) => Some(*b),
        Val::Num(n) => Some(*n != 0.0),
        Val::Str(s) => Some(!s.is_empty()),
        _ => None,
    }
}

fn sides(sheet: &Sheet, v: &Val) -> Option<[f32; 4]> {
    let l: Vec<f32> = match v {
        Val::List(l) => l.iter().map(|x| resolve_num(sheet, x)).collect::<Option<_>>()?,
        o => vec![resolve_num(sheet, o)?],
    };
    Some(match l.len() {
        1 => [l[0]; 4],
        2 => [l[0], l[1], l[0], l[1]],
        4 => [l[0], l[1], l[2], l[3]],
        _ => return None,
    })
}

/// The side (`style::side`) a property names after `prefix`: `padding-left`, `margin-top`, `left`.
fn side_named(k: &str, prefix: &str) -> Option<usize> {
    use crate::style::side;
    Some(match k.strip_prefix(prefix)? {
        "top" => side::TOP,
        "right" => side::RIGHT,
        "bottom" => side::BOTTOM,
        "left" => side::LEFT,
        _ => return None,
    })
}

/// A Path element's content changed by `f`, its outline parsed again; false when it does not parse.
fn path_mut(t: &mut crate::element::Tree, id: Id, f: impl FnOnce(&mut crate::style::PathContent)) -> bool {
    let Content::Path(mut p) = t.el(id).content().clone() else { return false };
    f(&mut p);
    let ok = match p.d.as_deref() {
        Some(d) => match crate::vector::parse(d, p.view) {
            Ok(shape) => {
                p.shape = shape;
                true
            }
            Err(_) => {
                p.shape = crate::vector::Shape::empty();
                false
            }
        },
        None => true,
    };
    t.set_content(id, Content::Path(p));
    ok
}

fn text_mut(ui: &mut Ui, id: Id, f: impl FnOnce(&mut TextContent)) -> bool {
    // A text field holds no text: its text's style is its text element's (the hint's too).
    if ui.io.edits.contains_key(&id) {
        ui.set_edit_style(id, f);
        return true;
    }
    let mut t = match &ui.tree.el(id).content() {
        Content::Text(t) => t.clone(),
        _ => TextContent::default(),
    };
    f(&mut t);
    ui.tree.set_content(id, Content::Text(t));
    true
}

/// Applies one property to an element. Returns false for an unknown property or a bad value.
#[cfg_attr(any(debug_assertions, test), track_caller)]
pub fn apply(ui: &mut Ui, sheet: &Sheet, id: Id, k: &str, v: &Val) -> bool {
    let t = &mut ui.tree;
    macro_rules! lay {
        ($f:ident, $val:expr) => {{
            let Some(x) = $val else { return false };
            t.set_layout(id, |l| l.$f = x);
            true
        }};
    }
    macro_rules! vis {
        ($f:ident, $val:expr) => {{
            let Some(x) = $val else { return false };
            t.set_visual(id, |vv| vv.$f = x);
            true
        }};
    }
    let color = |v: &Val| resolve_color(sheet, v);
    let num = |v: &Val| resolve_num(sheet, v);
    let len = |v: &Val| resolve_len(sheet, v);
    match k {
        "width" => lay!(width, len(v)),
        "height" => lay!(height, len(v)),
        // A text field's hint (a key's text is looked up again when the locale changes).
        "hint" if ui.io.edits.contains_key(&id) => {
            let Some(s) = hint_text(ui, v) else { return false };
            ui.set_edit_hint(id, &s);
            true
        }
        // One word, one concept (novaui-spec, "The .nui language"): `size` is a square box, on any element;
        // a text's size is `font-size`, an icon's or a path's `icon-size`.
        "size" => {
            let Some(x) = len(v) else { return false };
            t.set_layout(id, |l| {
                l.width = x;
                l.height = x;
            });
            true
        }
        "min-width" => lay!(min_width, len(v)),
        "min-height" => lay!(min_height, len(v)),
        "max-width" => lay!(max_width, len(v)),
        "max-height" => lay!(max_height, len(v)),
        "grow" => lay!(grow, num(v)),
        "shrink" => lay!(shrink, num(v)),
        "basis" => lay!(basis, len(v)),
        "gap" => lay!(gap, num(v)),
        "aspect" => lay!(aspect, num(v).map(Some)),
        "wrap-children" => lay!(wrap, truthy(v)),
        "padding" => lay!(padding, sides(sheet, v)),
        "padding-left" | "padding-right" | "padding-top" | "padding-bottom" => {
            let Some(x) = num(v) else { return false };
            let Some(i) = side_named(k, "padding-") else { return false };
            t.set_layout(id, |l| l.padding[i] = x);
            true
        }
        "margin" => {
            let Some(s) = sides(sheet, v) else { return false };
            t.set_layout(id, |l| l.margin = s.map(Len::Px));
            true
        }
        "margin-left" | "margin-right" | "margin-top" | "margin-bottom" => {
            let Some(x) = len(v) else { return false };
            let Some(i) = side_named(k, "margin-") else { return false };
            t.set_layout(id, |l| l.margin[i] = x);
            true
        }
        "left" | "right" | "top" | "bottom" => {
            let Some(x) = len(v) else { return false };
            let Some(i) = side_named(k, "") else { return false };
            t.set_layout(id, |l| {
                l.inset[i] = x;
                l.position = Position::Absolute;
            });
            true
        }
        "position" => {
            let p = match v {
                Val::Ident(s) if s == "absolute" => Position::Absolute,
                Val::Ident(s) if s == "relative" => Position::Relative,
                _ => return false,
            };
            t.set_layout(id, |l| l.position = p);
            true
        }
        "direction" => {
            let d = match v {
                Val::Ident(s) if s == "row" => FlexDirection::Row,
                Val::Ident(s) if s == "column" => FlexDirection::Column,
                _ => return false,
            };
            t.set_layout(id, |l| l.direction = d);
            true
        }
        "shown" => {
            let on = match v {
                Val::Ident(s) if s == "none" => false,
                Val::Ident(s) if s == "flex" => true,
                o => match truthy(o) {
                    Some(b) => b,
                    None => return false,
                },
            };
            t.set_layout(id, |l| l.display = if on { Display::Flex } else { Display::None });
            true
        }
        "align" | "align-self" => {
            let a = match v {
                Val::Ident(s) => match s.as_str() {
                    "start" => AlignItems::Start,
                    "center" => AlignItems::Center,
                    "end" => AlignItems::End,
                    "stretch" => AlignItems::Stretch,
                    "baseline" => AlignItems::Baseline,
                    _ => return false,
                },
                _ => return false,
            };
            if k == "align" {
                t.set_layout(id, |l| l.align_items = Some(a));
            } else {
                t.set_layout(id, |l| l.align_self = Some(a));
            }
            true
        }
        "justify" => {
            let j = match v {
                Val::Ident(s) => match s.as_str() {
                    "start" => JustifyContent::Start,
                    "center" => JustifyContent::Center,
                    "end" => JustifyContent::End,
                    "between" => JustifyContent::SpaceBetween,
                    "around" => JustifyContent::SpaceAround,
                    "evenly" => JustifyContent::SpaceEvenly,
                    _ => return false,
                },
                _ => return false,
            };
            t.set_layout(id, |l| l.justify = Some(j));
            true
        }
        "bg" => vis!(bg, color(v).map(|c| (c[3] > 0.0).then_some(c))),
        "bg-hover" => vis!(bg_hover, color(v).map(Some)),
        "bg-press" => vis!(bg_press, color(v).map(Some)),
        "bg-lit" => vis!(bg_lit, color(v).map(Some)),
        "bg-lit-press" => vis!(bg_lit_press, color(v).map(Some)),
        "bg-focus" => vis!(bg_focus, color(v).map(Some)),
        "border-focus" => vis!(border_focus, color(v).map(Some)),
        "underline" => vis!(underline, color(v).map(Some)),
        "underline-focus" => vis!(underline_focus, color(v).map(Some)),
        "border" => {
            let (w, c) = match v {
                Val::List(l) if l.len() == 2 => (num(&l[0]), color(&l[1])),
                o => (num(o), Some(t.el(id).visual().border_color)),
            };
            let (Some(w), Some(c)) = (w, c) else { return false };
            t.set_visual(id, |vv| {
                vv.border = w;
                vv.border_color = c;
            });
            true
        }
        "border-color" => vis!(border_color, color(v)),
        "radius" => vis!(radius, num(v)),
        // Which corners `radius` rounds (the others square): all, top, bottom, left, right, none.
        "corners" => {
            use crate::render::primitive::corner;
            let round = match v {
                Val::Ident(s) => match s.as_str() {
                    "all" => Some(corner::ALL),
                    "top" => Some(corner::TOP),
                    "bottom" => Some(corner::BOTTOM),
                    "left" => Some(corner::LEFT),
                    "right" => Some(corner::RIGHT),
                    "none" => Some(0),
                    _ => None,
                },
                _ => None,
            };
            vis!(square, round.map(|r| corner::ALL & !r))
        }
        "shadow" => vis!(shadow, resolve_shadow(sheet, v)),
        "material" => {
            let m = match v {
                Val::Ident(s) if s == "none" => Some(None),
                Val::Ident(s) => match builtin(s) {
                    Some(Tok::Material(m)) => Some(Some(m)),
                    _ => None,
                },
                _ => None,
            };
            vis!(material, m)
        }
        "fg" => vis!(fg, color(v)),
        // The fg's alpha alone (an ink level computed from state, the colour kept).
        "fg-alpha" => {
            let Some(a) = num(v) else { return false };
            t.set_visual(id, |vv| vv.fg[3] = a);
            true
        }
        // A text's karaoke sweep: chars sung (negative: none), and the sung chars' colour.
        "sweep" => {
            let Some(n) = num(v) else { return false };
            t.set_visual(id, |vv| vv.sweep = (n >= 0.0).then_some(n));
            true
        }
        // A text's units (`word`, `char`, `none`), their timing (`[first char, from, to, …]`,
        // seconds) and the time now: what the `u-*` expressions are evaluated for, per unit.
        "units" => {
            let words = match v {
                Val::Ident(s) | Val::Str(s) if s == "word" => Some(true),
                Val::Ident(s) | Val::Str(s) if s == "char" => Some(false),
                Val::Ident(s) | Val::Str(s) if s == "none" => None,
                _ => return false,
            };
            t.set_unit_src(id, |u| u.words = words);
            true
        }
        "timing" => {
            let l: &[Val] = match v {
                Val::List(l) => l,
                o => std::slice::from_ref(o),
            };
            let n: Option<Vec<f32>> = l.iter().map(num).collect();
            let Some(n) = n else { return false };
            let spans: std::rc::Rc<[(u32, f32, f32)]> =
                n.chunks_exact(3).map(|s| (s[0].max(0.0) as u32, s[1], s[2])).collect();
            t.set_unit_src(id, |u| u.timing = spans);
            true
        }
        "time" => {
            let Some(n) = num(v) else { return false };
            t.set_unit_src(id, |u| u.time = n);
            true
        }
        "sweep-fg" => vis!(sweep_fg, color(v)),
        // A text turned about its centre (degrees clockwise, ±90), paint only.
        "text-rotate" => vis!(text_rotate, num(v).map(|a| if a.is_finite() { a.clamp(-90.0, 90.0) } else { 0.0 })),
        // Where it turns about: dips from the text's centre, each within ±4096.
        "text-origin" => {
            let ok = |v: Option<f32>| v.filter(|v| v.is_finite()).map(|v| v.clamp(-4096.0, 4096.0));
            let xy = match v {
                Val::List(l) if l.len() == 2 => ok(num(&l[0])).zip(ok(num(&l[1]))),
                o => ok(num(o)).map(|x| (x, 0.0)),
            };
            vis!(text_origin, xy.map(|(x, y)| [x, y]))
        }
        "sweep-alpha" => {
            let Some(a) = num(v) else { return false };
            t.set_visual(id, |vv| vv.sweep_fg[3] = a);
            true
        }
        // Moves the element as drawn (layout keeps its place): `translate: x, y` in dips.
        "translate" => {
            let (x, y) = match v {
                Val::List(l) if l.len() == 2 => (num(&l[0]), num(&l[1])),
                o => (Some(0.0), num(o)),
            };
            let (Some(x), Some(y)) = (x, y) else { return false };
            t.set_translate(id, x, y);
            true
        }
        "fg-hover" => vis!(fg_hover, color(v).map(Some)),
        "fg-disabled" => vis!(fg_disabled, color(v).map(Some)),
        "fg-off" => vis!(fg_off, color(v).map(Some)),
        "opacity" => vis!(opacity, num(v)),
        "clip" => vis!(clip, truthy(v)),
        "visible" => vis!(visible, truthy(v)),
        "hit" => vis!(hit, truthy(v)),
        "scale" => vis!(scale, num(v)),
        "scale-x" => vis!(scale_x, num(v)),
        "underline-hover" => vis!(underline_hover, truthy(v)),
        "cursor" => {
            let c = match v {
                Val::Ident(s) => match s.as_str() {
                    "arrow" => Cursor::Arrow,
                    "hand" => Cursor::Hand,
                    "text" => Cursor::Text,
                    "grab" => Cursor::Grab,
                    _ => return false,
                },
                _ => return false,
            };
            t.set_visual(id, |vv| vv.cursor = c);
            true
        }
        "layer" => {
            let Some(b) = truthy(v) else { return false };
            t.set_promoted(id, b);
            true
        }
        "on-click" => match v {
            Val::Ident(s) | Val::Str(s) => {
                let s = s.clone();
                t.set_action(id, Some(&s));
                t.set_visual(id, |vv| vv.hit = true);
                true
            }
            _ => false,
        },
        // What the element is to the keyboard and to assistive clients (`semantics.rs`).
        "role" => {
            let (Val::Ident(s) | Val::Str(s)) = v else { return false };
            let Some(r) = crate::semantics::Role::parse(s) else { return false };
            t.set_semantics(id, |a| a.role = Some(r));
            true
        }
        "label" => {
            let s = match v {
                Val::Str(s) => s.clone(),
                Val::Key(k) => ui.t(k, &[]),
                _ => return false,
            };
            ui.tree.set_semantics(id, |a| a.label = Some(s));
            true
        }
        "focusable" => {
            let Some(b) = truthy(v) else { return false };
            t.set_semantics(id, |a| a.focusable = Some(b));
            true
        }
        "live" => {
            let Some(b) = truthy(v) else { return false };
            t.set_semantics(id, |a| a.live = b);
            true
        }
        "value" | "min" | "max" | "step" => {
            let Some(n) = num(v) else { return false };
            let n = n as f64;
            t.set_semantics(id, |a| {
                let r = a.range.get_or_insert_with(Default::default);
                match k {
                    "value" => r.value = n,
                    "min" => r.min = n,
                    "max" => r.max = n,
                    _ => r.step = n,
                }
            });
            true
        }
        "hover-events" | "press-events" | "wheel-events" | "drag-events" => {
            let Some(b) = truthy(v) else { return false };
            let listen = match k {
                "hover-events" => crate::element::Listen::Hover,
                "press-events" => crate::element::Listen::Press,
                "wheel-events" => crate::element::Listen::Wheel,
                _ => crate::element::Listen::Drag,
            };
            t.set_listen(id, listen, b);
            true
        }
        "lit" => {
            let Some(b) = truthy(v) else { return false };
            t.set_lit(id, b);
            true
        }
        "disabled" => {
            let Some(b) = truthy(v) else { return false };
            t.set_disabled(id, b);
            true
        }
        "off" => {
            let Some(b) = truthy(v) else { return false };
            t.set_off(id, b);
            true
        }
        "tag" => {
            let Some(n) = num(v) else { return false };
            t.set_tag(id, n as u64);
            true
        }
        // text
        "text" => {
            let s = match v {
                Val::Str(s) => s.clone(),
                Val::Key(k) => ui.t(k, &[]),
                Val::Num(n) => format!("{n}"),
                Val::Ident(s) => s.clone(),
                _ => return false,
            };
            text_mut(ui, id, |tc| tc.text = s)
        }
        "type" => {
            let Some((sz, ln, w)) = resolve_type(sheet, v) else { return false };
            text_mut(ui, id, |tc| {
                tc.size = sz;
                tc.line = ln;
                tc.weight = w;
            })
        }
        "icon-size" if matches!(t.el(id).content(), Content::Path(_)) => {
            let Some(n) = num(v) else { return false };
            path_mut(t, id, |p| p.size = n)
        }
        // A Path's box (`view`, default 24) and outline (`d`): parsed when set; a bad outline
        // is refused (the element draws nothing).
        "view" if matches!(t.el(id).content(), Content::Path(_)) => {
            let Some(n) = num(v) else { return false };
            path_mut(t, id, |p| p.view = n)
        }
        "d" if matches!(t.el(id).content(), Content::Path(_)) => {
            let Val::Str(s) = v else { return false };
            let s: std::sync::Arc<str> = std::sync::Arc::from(s.as_str());
            path_mut(t, id, |p| p.d = Some(s))
        }
        "font-size" if !matches!(t.el(id).content(), Content::Icon(_) | Content::Path(_)) => {
            let Some(n) = num(v) else { return false };
            text_mut(ui, id, |tc| tc.size = n)
        }
        "line" => {
            let Some(n) = num(v) else { return false };
            text_mut(ui, id, |tc| tc.line = n)
        }
        "weight" => {
            let Some(n) = num(v) else { return false };
            text_mut(ui, id, |tc| tc.weight = n as u16)
        }
        "lines" => {
            let Some(n) = num(v) else { return false };
            text_mut(ui, id, |tc| {
                tc.lines = n as u16;
                tc.wrap = n != 1.0;
            })
        }
        "text-align" => {
            let a = match v {
                Val::Ident(s) => match s.as_str() {
                    "start" | "left" => crate::text::Align::Start,
                    "center" => crate::text::Align::Center,
                    "end" | "right" => crate::text::Align::End,
                    _ => return false,
                },
                _ => return false,
            };
            text_mut(ui, id, |tc| tc.align = a)
        }
        "icon" => {
            let ic = match v {
                Val::Ident(s) | Val::Str(s) => match icon_by_name(s) {
                    Some(i) => i,
                    None => return false,
                },
                _ => return false,
            };
            let (size, cut) = match &t.el(id).content() {
                Content::Icon(c) => (c.size, c.cut),
                _ => (16.0, None),
            };
            t.set_content(id, Content::Icon(IconContent { icon: ic, size, cut, named: true }));
            true
        }
        // `icon-cut: TextQuote 80`: an icon knocked out of this one, at a percent of its size.
        "icon-cut" => {
            let (name, pct) = match v {
                Val::List(l) if l.len() == 2 => (l[0].clone(), num(&l[1]).unwrap_or(80.0)),
                o => (o.clone(), 80.0),
            };
            let cut = match &name {
                Val::Ident(s) | Val::Str(s) => icon_by_name(s).map(|i| (i, pct.clamp(1.0, 100.0) as u8)),
                _ => None,
            };
            let ic = match &t.el(id).content() {
                Content::Icon(c) => IconContent { cut, ..*c },
                _ => return false,
            };
            t.set_content(id, Content::Icon(ic));
            true
        }
        "icon-size" => {
            let Some(n) = num(v) else { return false };
            let ic = match &t.el(id).content() {
                Content::Icon(c) => IconContent { size: n, ..*c },
                // A style may give the size before the element names its icon.
                Content::None => IconContent { icon: Icon::ALL[0], size: n, cut: None, named: false },
                _ => return false,
            };
            t.set_content(id, Content::Icon(ic));
            true
        }
        _ => false,
    }
}
