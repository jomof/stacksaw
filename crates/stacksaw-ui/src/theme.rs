//! The UI theme: colors, glyphs, and modifiers, loaded from the embedded
//! `theme.toml` (§8.3). Styling values live in data, not code; this module
//! parses that file once, resolves the cascade (`base` → `extends` → role →
//! state), and hands the renderer ready `ratatui::Style`s and glyphs.
//!
//! Rainbow foregrounds carry an *identity source* (`stack`, `file_dir`,
//! `commit`); the renderer supplies the per-element hue input via
//! [`RainbowInput`], and the theme turns it into a terminal color honoring the
//! terminal's depth.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use stacksaw_rainbox::{
    ansi256_to_rgb, golden_angle_hue, staircase_arc_hue, Background, DimCurve, RainboxColor,
    StaircaseArc,
};

/// The embedded theme source. Parsed once at startup; see [`Theme::load`].
const THEME_TOML: &str = include_str!("../theme.toml");

/// A glyph-only overlay deep-merged onto [`THEME_TOML`] when the user opts into
/// Nerd Font glyphs. See [`Theme::load_with`] and `theme-nerd.toml`.
const THEME_NERD_TOML: &str = include_str!("../theme-nerd.toml");

/// Which set of glyphs the UI draws. `Unicode` uses only widely-supported BMP
/// symbols (the default, legible on any terminal); `Nerd` overlays Nerd Font
/// codepoints for richer git/status marks, and requires a patched terminal font.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphSet {
    Unicode,
    Nerd,
}

impl GlyphSet {
    /// Map a config/env string to a glyph set. Anything but `nerd` (case
    /// -insensitive) is the safe Unicode default, so a typo never yields tofu.
    pub fn from_str(s: &str) -> GlyphSet {
        if s.trim().eq_ignore_ascii_case("nerd") {
            GlyphSet::Nerd
        } else {
            GlyphSet::Unicode
        }
    }
}

/// Render context: the terminal's color depth and perceptual background, needed
/// to lower a [`ColorSpec`] to a concrete [`Color`].
#[derive(Clone, Copy)]
pub struct Ctx {
    pub truecolor: bool,
    pub background: Background,
}

/// The hue input for a rainbow foreground, supplied per element by the
/// renderer. `Key` feeds a `hash` identity source; `Position` feeds an `arc`.
#[derive(Clone, Copy)]
pub enum RainbowInput<'a> {
    None,
    Key(&'a str),
    Position { index: usize, total: usize },
}

/// The four commit status chips (§8.3), keyed the same in content and legend.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Clean,
    Error,
    Warning,
    Twin,
}

impl ChipKind {
    fn key(self) -> &'static str {
        match self {
            ChipKind::Clean => "clean",
            ChipKind::Error => "error",
            ChipKind::Warning => "warning",
            ChipKind::Twin => "twin",
        }
    }
}

// ── Raw (serde) shapes ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    palette: HashMap<String, RawPaletteEntry>,
    #[serde(default)]
    identity: HashMap<String, RawIdentity>,
    #[serde(default)]
    rainbow: RawRainbow,
    #[serde(default)]
    base: RawStyle,
    #[serde(default)]
    role: HashMap<String, toml::Value>,
    #[serde(default)]
    diff: RawDiff,
    #[serde(default)]
    state: HashMap<String, RawStyle>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPaletteEntry {
    Simple(RawColor),
    Facets {
        fg: RawColor,
        #[serde(default)]
        bg: Option<RawColor>,
    },
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum RawColor {
    Str(String),
    Fallback { truecolor: String, ansi256: String },
    Rainbow { rainbow: String },
}

#[derive(Deserialize)]
struct RawIdentity {
    mode: String,
    #[serde(default)]
    #[allow(dead_code)]
    key: Option<String>,
}

#[derive(Deserialize)]
struct RawRainbow {
    lightness: f32,
    chroma: f32,
    relevance: f32,
    contrast_floor: f32,
    dim: RawDim,
    arc: RawArc,
    #[allow(dead_code)]
    background: RawBackground,
}

impl Default for RawRainbow {
    fn default() -> Self {
        RawRainbow {
            lightness: 0.72,
            chroma: 0.18,
            relevance: 1.0,
            contrast_floor: DimCurve::default().contrast_floor,
            dim: RawDim::default(),
            arc: RawArc::default(),
            background: RawBackground::default(),
        }
    }
}

#[derive(Deserialize)]
struct RawDim {
    lightness_toward_bg: f32,
    chroma: f32,
}

impl Default for RawDim {
    fn default() -> Self {
        let d = DimCurve::default();
        RawDim { lightness_toward_bg: d.lightness_toward_bg, chroma: d.chroma }
    }
}

#[derive(Deserialize)]
struct RawArc {
    h0_deg: f32,
    span_deg: f32,
}

impl Default for RawArc {
    fn default() -> Self {
        RawArc { h0_deg: 250.0, span_deg: -190.0 }
    }
}

#[derive(Deserialize)]
struct RawBackground {
    #[allow(dead_code)]
    mode: String,
    #[allow(dead_code)]
    dark: f32,
    #[allow(dead_code)]
    light: f32,
}

impl Default for RawBackground {
    fn default() -> Self {
        RawBackground { mode: "auto".into(), dark: 0.14, light: 0.96 }
    }
}

#[derive(Deserialize, Default)]
struct RawDiff {
    #[serde(default)]
    syntax_theme: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
struct RawStyle {
    extends: Option<String>,
    fg: Option<RawColor>,
    bg: Option<RawColor>,
    glyph: Option<String>,
    lead: Option<String>,
    trail: Option<String>,
    title: Option<Box<RawStyle>>,
    bold: Option<bool>,
    dim: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    reversed: Option<bool>,
}

#[derive(Deserialize)]
struct RawChip {
    clean: RawChipEntry,
    error: RawChipEntry,
    warning: RawChipEntry,
    twin: RawChipEntry,
}

#[derive(Deserialize)]
struct RawChipEntry {
    glyph: String,
    fg: RawColor,
}

// ── Resolved model ───────────────────────────────────────────────────────────

/// An unresolved color: still needs a [`Ctx`] (and, for `Rainbow`, a
/// [`RainbowInput`]) to become a concrete terminal [`Color`].
#[derive(Clone)]
enum ColorSpec {
    /// Terminal default; contributes no `fg`/`bg` to the style.
    Default,
    Fixed(Color),
    Fallback { truecolor: Color, ansi256: Color },
    Rainbow(String),
}

/// A palette token: a foreground color and an optional background facet.
#[derive(Clone)]
struct PaletteColor {
    fg: ColorSpec,
    bg: Option<ColorSpec>,
}

/// A hue source: either a hashed key or a position along the staircase arc.
enum IdentityMode {
    Hash,
    Arc,
}

/// The five modifier flags as tri-state (unset inherits).
#[derive(Clone, Copy, Default)]
struct Mods {
    bold: Option<bool>,
    dim: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    reversed: Option<bool>,
}

impl Mods {
    fn from_raw(r: &RawStyle) -> Mods {
        Mods {
            bold: r.bold,
            dim: r.dim,
            italic: r.italic,
            underline: r.underline,
            reversed: r.reversed,
        }
    }

    /// Overlay `o` onto `self`: each flag set in `o` wins.
    fn overlay(self, o: Mods) -> Mods {
        Mods {
            bold: o.bold.or(self.bold),
            dim: o.dim.or(self.dim),
            italic: o.italic.or(self.italic),
            underline: o.underline.or(self.underline),
            reversed: o.reversed.or(self.reversed),
        }
    }

    fn modifier(self) -> Modifier {
        let mut m = Modifier::empty();
        if self.bold == Some(true) {
            m |= Modifier::BOLD;
        }
        if self.dim == Some(true) {
            m |= Modifier::DIM;
        }
        if self.italic == Some(true) {
            m |= Modifier::ITALIC;
        }
        if self.underline == Some(true) {
            m |= Modifier::UNDERLINED;
        }
        if self.reversed == Some(true) {
            m |= Modifier::REVERSED;
        }
        m
    }
}

/// A fully-cascaded role style (extends flattened, palette resolved).
#[derive(Clone)]
struct RoleStyle {
    fg: ColorSpec,
    bg: ColorSpec,
    glyph: Option<String>,
    lead: Option<String>,
    trail: Option<String>,
    mods: Mods,
}

impl Default for RoleStyle {
    fn default() -> Self {
        RoleStyle {
            fg: ColorSpec::Default,
            bg: ColorSpec::Default,
            glyph: None,
            lead: None,
            trail: None,
            mods: Mods::default(),
        }
    }
}

/// A resolved state layer: fields applied directly, plus an optional `title`
/// region override (chrome text styled apart from the row body).
#[derive(Default)]
struct StateStyle {
    node: StateNode,
    title: Option<StateNode>,
}

/// A resolved state node. Unset `fg`/`bg` mean "leave as-is" (unlike a role,
/// which inherits the base default), since states overlay existing styles.
#[derive(Default)]
struct StateNode {
    fg: Option<ColorSpec>,
    bg: Option<ColorSpec>,
    glyph: Option<String>,
    mods: Mods,
}

/// The resolved theme: the single source of truth for UI styling.
pub struct Theme {
    identity: HashMap<String, IdentityMode>,
    rainbow_lightness: f32,
    rainbow_chroma: f32,
    rainbow_relevance: f32,
    dim: DimCurve,
    arc: StaircaseArc,
    /// The cascade root (`[base]`): the default under every role, and the fill
    /// painted behind the whole scene (its `bg`).
    base: RoleStyle,
    roles: HashMap<String, RoleStyle>,
    /// Per-role state variants (`[role.<id>.<state>]`): a delta overlaid on the
    /// base role style when the element is in that state. Inherited through
    /// `extends`, so a shared class's variant reaches every role that extends it.
    role_variants: HashMap<String, HashMap<String, StateNode>>,
    chips: HashMap<&'static str, (String, ColorSpec)>,
    file_status: HashMap<String, ColorSpec>,
    states: HashMap<String, StateStyle>,
    syntax_theme: String,
}

impl Theme {
    /// Load and resolve the embedded theme with the default (Unicode) glyphs.
    /// Panics only if the embedded file is malformed, which a unit test guards
    /// against.
    pub fn load() -> Theme {
        Theme::load_with(GlyphSet::Unicode)
    }

    /// Load and resolve the theme for `glyphs`. The Nerd set deep-merges the
    /// `theme-nerd.toml` glyph overlay onto the base at the raw-TOML level
    /// before the cascade resolves, so only `glyph` fields differ — every color
    /// and modifier still comes from `theme.toml`.
    pub fn load_with(glyphs: GlyphSet) -> Theme {
        let mut value: toml::Value =
            toml::from_str(THEME_TOML).expect("embedded theme.toml parses");
        if glyphs == GlyphSet::Nerd {
            let overlay: toml::Value =
                toml::from_str(THEME_NERD_TOML).expect("embedded theme-nerd.toml parses");
            merge_toml(&mut value, overlay);
        }
        let raw: Raw = value.try_into().expect("merged theme has the expected shape");
        Theme::build(raw)
    }

    fn build(raw: Raw) -> Theme {
        // 1. Palette (no palette-refs within palette).
        let mut palette: HashMap<String, PaletteColor> = HashMap::new();
        for (name, entry) in &raw.palette {
            let pc = match entry {
                RawPaletteEntry::Simple(c) => PaletteColor {
                    fg: spec(&HashMap::new(), c),
                    bg: None,
                },
                RawPaletteEntry::Facets { fg, bg } => PaletteColor {
                    fg: spec(&HashMap::new(), fg),
                    bg: bg.as_ref().map(|c| spec(&HashMap::new(), c)),
                },
            };
            palette.insert(name.clone(), pc);
        }

        // 2. Identity sources.
        let identity = raw
            .identity
            .iter()
            .map(|(k, v)| {
                let mode = if v.mode == "arc" { IdentityMode::Arc } else { IdentityMode::Hash };
                (k.clone(), mode)
            })
            .collect();

        // 3. Split role tables into style roles, chips, and file-status. A
        //    style role may carry `[role.<id>.<state>]` sub-tables (state
        //    variants), which are extracted here before the base is parsed.
        let mut role_raws: HashMap<String, RawStyle> = HashMap::new();
        let mut variant_raws: HashMap<String, HashMap<String, RawStyle>> = HashMap::new();
        let mut chips: HashMap<&'static str, (String, ColorSpec)> = HashMap::new();
        let mut file_status: HashMap<String, ColorSpec> = HashMap::new();
        for (name, val) in raw.role {
            match name.as_str() {
                "chip" => {
                    let c: RawChip = val.try_into().expect("[role.chip] shape");
                    for (kind, e) in [
                        ("clean", c.clean),
                        ("error", c.error),
                        ("warning", c.warning),
                        ("twin", c.twin),
                    ] {
                        chips.insert(kind, (e.glyph, spec(&palette, &e.fg)));
                    }
                }
                "file_status" => {
                    let fs: HashMap<String, RawColor> =
                        val.try_into().expect("[role.file_status] shape");
                    for (k, c) in &fs {
                        file_status.insert(k.clone(), spec(&palette, c));
                    }
                }
                _ => {
                    // A sub-table whose key is not a style field is a state
                    // variant (e.g. `[role.row_text.row_selected]`).
                    if let Some(table) = val.as_table() {
                        let mut variants: HashMap<String, RawStyle> = HashMap::new();
                        for (key, sub) in table {
                            if !ROLE_FIELDS.contains(&key.as_str()) && sub.is_table() {
                                let rs: RawStyle =
                                    sub.clone().try_into().expect("role state variant shape");
                                variants.insert(key.clone(), rs);
                            }
                        }
                        if !variants.is_empty() {
                            variant_raws.insert(name.clone(), variants);
                        }
                    }
                    let rs: RawStyle = val.try_into().expect("role shape");
                    role_raws.insert(name, rs);
                }
            }
        }

        // 4. Resolve each role's cascade (base → extends chain → self), then its
        //    state variants (inherited through the same chain).
        let base = role_style_from(&palette, &RoleStyle::default(), &raw.base);
        let mut roles: HashMap<String, RoleStyle> = HashMap::new();
        let names: Vec<String> = role_raws.keys().cloned().collect();
        for name in &names {
            let mut stack: Vec<String> = Vec::new();
            resolve_role(name, &role_raws, &base, &palette, &mut roles, &mut stack);
        }
        let mut role_variants: HashMap<String, HashMap<String, StateNode>> = HashMap::new();
        for name in &names {
            let mut stack: Vec<String> = Vec::new();
            let merged = merged_variants(name, &role_raws, &variant_raws, &mut stack);
            if !merged.is_empty() {
                let nodes = merged
                    .iter()
                    .map(|(state, delta)| (state.clone(), state_node(&palette, delta)))
                    .collect();
                role_variants.insert(name.clone(), nodes);
            }
        }

        // 5. States (with an optional title region).
        let states = raw
            .state
            .iter()
            .map(|(name, rs)| {
                let style = StateStyle {
                    node: state_node(&palette, rs),
                    title: rs.title.as_ref().map(|t| state_node(&palette, t)),
                };
                (name.clone(), style)
            })
            .collect();

        Theme {
            identity,
            rainbow_lightness: raw.rainbow.lightness,
            rainbow_chroma: raw.rainbow.chroma,
            rainbow_relevance: raw.rainbow.relevance,
            dim: DimCurve {
                lightness_toward_bg: raw.rainbow.dim.lightness_toward_bg,
                chroma: raw.rainbow.dim.chroma,
                contrast_floor: raw.rainbow.contrast_floor,
            },
            base,
            arc: StaircaseArc {
                h0_deg: raw.rainbow.arc.h0_deg,
                span_deg: raw.rainbow.arc.span_deg,
            },
            roles,
            role_variants,
            chips,
            file_status,
            states,
            syntax_theme: raw
                .diff
                .syntax_theme
                .unwrap_or_else(|| "base16-ocean.dark".into()),
        }
    }

    // --- Renderer-facing accessors ---------------------------------------

    /// The syntect theme name for diff highlighting.
    pub fn syntax_theme(&self) -> &str {
        &self.syntax_theme
    }

    /// The scene background fill from `[base].bg`, or `None` for the terminal's
    /// own background (`bg = "default"`). Painted behind the whole UI so an empty
    /// cell shows this rather than whatever the terminal happens to use.
    pub fn background(&self, ctx: Ctx) -> Option<Color> {
        self.color(&self.base.bg, ctx, RainbowInput::None)
    }

    /// The glyph for `role`, or `""` if it has none.
    pub fn glyph(&self, role: &str) -> &str {
        self.roles
            .get(role)
            .and_then(|r| r.glyph.as_deref())
            .unwrap_or("")
    }

    /// The `lead`/`trail` fragments of a composite role (e.g. `segment_riser`).
    pub fn lead(&self, role: &str) -> &str {
        self.roles.get(role).and_then(|r| r.lead.as_deref()).unwrap_or("")
    }
    pub fn trail(&self, role: &str) -> &str {
        self.roles.get(role).and_then(|r| r.trail.as_deref()).unwrap_or("")
    }

    /// The full `ratatui` style for `role`, resolving any rainbow color with
    /// `rb` and lowering colors for `ctx`.
    pub fn style(&self, role: &str, ctx: Ctx, rb: RainbowInput) -> Style {
        let rs = self.roles.get(role).cloned().unwrap_or_default();
        let mut style = Style::default();
        if let Some(c) = self.color(&rs.fg, ctx, rb) {
            style = style.fg(c);
        }
        if let Some(c) = self.color(&rs.bg, ctx, rb) {
            style = style.bg(c);
        }
        style.add_modifier(rs.mods.modifier())
    }

    /// Like [`style`](Self::style), but resolves the role's rainbow color(s) at
    /// `relevance` rather than the fixed global one, so an element can fade by
    /// its own signal (e.g. a recents row by MRU age) while keeping the hue its
    /// identity picks. Non-rainbow fields are unaffected.
    pub fn style_at(&self, role: &str, ctx: Ctx, rb: RainbowInput, relevance: f32) -> Style {
        let rs = self.roles.get(role).cloned().unwrap_or_default();
        let mut style = Style::default();
        if let Some(c) = self.color_at(&rs.fg, ctx, rb, relevance) {
            style = style.fg(c);
        }
        if let Some(c) = self.color_at(&rs.bg, ctx, rb, relevance) {
            style = style.bg(c);
        }
        style.add_modifier(rs.mods.modifier())
    }

    /// Like [`style`](Self::style), but overlays `role`'s variant for `state`
    /// (`[role.<id>.<state>]`) when one exists — e.g. brightening the plain
    /// row-text class on the selected row. Falls back to the base style.
    pub fn style_state(&self, role: &str, state: &str, ctx: Ctx, rb: RainbowInput) -> Style {
        let base = self.style(role, ctx, rb);
        match self.role_variants.get(role).and_then(|m| m.get(state)) {
            Some(node) => self.node_style(base, node, ctx),
            None => base,
        }
    }

    /// A chip's glyph and style (semantic color, no rainbow input needed).
    pub fn chip(&self, kind: ChipKind, ctx: Ctx) -> (String, Style) {
        match self.chips.get(kind.key()) {
            Some((glyph, spec)) => {
                let mut style = Style::default();
                if let Some(c) = self.color(spec, ctx, RainbowInput::None) {
                    style = style.fg(c);
                }
                (glyph.clone(), style)
            }
            None => (String::new(), Style::default()),
        }
    }

    /// The style for a git name-status letter (`A`/`M`/`D`/`R`/`C`, else other).
    pub fn file_status_style(&self, status: char, ctx: Ctx) -> Style {
        let key = match status {
            'A' => "added",
            'M' => "modified",
            'D' => "deleted",
            'R' => "renamed",
            'C' => "copied",
            _ => "other",
        };
        let spec = self.file_status.get(key).cloned().unwrap_or(ColorSpec::Default);
        let mut style = Style::default();
        if let Some(c) = self.color(&spec, ctx, RainbowInput::None) {
            style = style.fg(c);
        }
        style
    }

    /// The selected-row marker glyph (e.g. `"▶ "`).
    pub fn selection_symbol(&self) -> &str {
        self.states
            .get("row_selected")
            .and_then(|s| s.node.glyph.as_deref())
            .unwrap_or("")
    }

    /// The selected-row bar style (background + any modifiers). When the row's
    /// column is unfocused, the `row_selected_unfocused` delta lightens the bar
    /// so the selection stays legible without dimming the column's content.
    pub fn selection_style(&self, focused: bool, ctx: Ctx) -> Style {
        let base = self
            .states
            .get("row_selected")
            .map(|s| self.node_style(Style::default(), &s.node, ctx))
            .unwrap_or_default();
        if focused {
            return base;
        }
        self.states
            .get("row_selected_unfocused")
            .map(|s| self.node_style(base, &s.node, ctx))
            .unwrap_or(base)
    }

    /// The column title style, brightened when the column is focused.
    pub fn column_title_style(&self, focused: bool, ctx: Ctx) -> Style {
        let mut style = self.style("column_title", ctx, RainbowInput::None);
        if focused {
            if let Some(node) = self.states.get("column_focused").and_then(|s| s.title.as_ref()) {
                style = self.node_style(style, node, ctx);
            }
        }
        style
    }

    // --- Color resolution ------------------------------------------------

    fn color(&self, spec: &ColorSpec, ctx: Ctx, rb: RainbowInput) -> Option<Color> {
        self.color_at(spec, ctx, rb, self.rainbow_relevance)
    }

    /// Like [`color`](Self::color) but resolves any rainbow fg/bg at a
    /// caller-supplied `relevance` instead of the fixed global one. Relevance is
    /// orthogonal to hue: the identity still chooses the hue, `relevance` only
    /// fades it toward the background (§8.3).
    fn color_at(
        &self,
        spec: &ColorSpec,
        ctx: Ctx,
        rb: RainbowInput,
        relevance: f32,
    ) -> Option<Color> {
        match spec {
            ColorSpec::Default => None,
            ColorSpec::Fixed(c) => Some(self.dim_fixed(*c, relevance, ctx)),
            ColorSpec::Fallback { truecolor, ansi256 } => {
                let c = if ctx.truecolor { *truecolor } else { *ansi256 };
                Some(self.dim_fixed(c, relevance, ctx))
            }
            ColorSpec::Rainbow(src) => {
                Some(self.rainbow_color(self.hue(src, rb), relevance, ctx))
            }
        }
    }

    /// Fade a fixed (non-rainbow) color toward the background by `relevance`,
    /// using the same [`DimCurve`] as generated hues so a relevance-carrying
    /// element (e.g. an aging recents row) dims its plain text the same way it
    /// dims an identity hue. A no-op at full relevance, so every element without
    /// a relevance signal renders its exact themed color.
    fn dim_fixed(&self, c: Color, relevance: f32, ctx: Ctx) -> Color {
        if relevance >= 1.0 {
            return c;
        }
        let Some((r, g, b)) = color_to_rgb(c) else {
            return c;
        };
        let dimmed =
            RainboxColor::from_rgb(r, g, b).dimmed_with(relevance.clamp(0.0, 1.0), ctx.background, self.dim);
        if ctx.truecolor {
            let (r, g, b) = dimmed.to_rgb();
            Color::Rgb(r, g, b)
        } else {
            Color::Indexed(dimmed.to_ansi256())
        }
    }

    fn hue(&self, source: &str, rb: RainbowInput) -> f32 {
        match self.identity.get(source) {
            Some(IdentityMode::Hash) => match rb {
                RainbowInput::Key(k) => golden_angle_hue(k),
                _ => 0.0,
            },
            Some(IdentityMode::Arc) => match rb {
                RainbowInput::Position { index, total } => {
                    staircase_arc_hue(self.arc, index, total)
                }
                _ => 0.0,
            },
            None => 0.0,
        }
    }

    fn rainbow_color(&self, hue: f32, relevance: f32, ctx: Ctx) -> Color {
        let c = RainboxColor::new(self.rainbow_lightness, self.rainbow_chroma, hue)
            .dimmed_with(relevance.clamp(0.0, 1.0), ctx.background, self.dim);
        if ctx.truecolor {
            let (r, g, b) = c.to_rgb();
            Color::Rgb(r, g, b)
        } else {
            Color::Indexed(c.to_ansi256())
        }
    }

    /// Overlay a resolved state node onto `base`.
    fn node_style(&self, base: Style, node: &StateNode, ctx: Ctx) -> Style {
        let mut style = base;
        if let Some(spec) = &node.fg {
            if let Some(c) = self.color(spec, ctx, RainbowInput::None) {
                style = style.fg(c);
            }
        }
        if let Some(spec) = &node.bg {
            if let Some(c) = self.color(spec, ctx, RainbowInput::None) {
                style = style.bg(c);
            }
        }
        style.add_modifier(node.mods.modifier())
    }
}

// ── Build helpers ─────────────────────────────────────────────────────────────

/// Recursively merge `overlay` into `base`: tables merge key-by-key, any other
/// value overwrites. Used to lay the Nerd glyph overlay over the base theme
/// before the cascade resolves, so an overlay entry replaces only the fields it
/// names (e.g. a chip's `glyph`) and leaves its siblings (`fg`) intact.
fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge_toml(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Resolve a [`RawColor`] to a [`ColorSpec`], expanding `palette.<token>[.bg]`.
fn spec(palette: &HashMap<String, PaletteColor>, c: &RawColor) -> ColorSpec {
    match c {
        RawColor::Str(s) => {
            if s == "default" {
                ColorSpec::Default
            } else if let Some(rest) = s.strip_prefix("palette.") {
                let (name, bg) = match rest.strip_suffix(".bg") {
                    Some(n) => (n, true),
                    None => (rest, false),
                };
                match palette.get(name) {
                    Some(pc) if bg => pc.bg.clone().unwrap_or(ColorSpec::Default),
                    Some(pc) => pc.fg.clone(),
                    None => ColorSpec::Default,
                }
            } else {
                ColorSpec::Fixed(concrete(s))
            }
        }
        RawColor::Fallback { truecolor, ansi256 } => ColorSpec::Fallback {
            truecolor: concrete(truecolor),
            ansi256: concrete(ansi256),
        },
        RawColor::Rainbow { rainbow } => ColorSpec::Rainbow(rainbow.clone()),
    }
}

/// Resolve a terminal [`Color`] to 8-bit sRGB so it can be dimmed in OKLCH.
/// Named colors map through their xterm palette index; `Reset`/`Default` and
/// anything without a concrete value return `None` (left undimmed).
fn color_to_rgb(c: Color) -> Option<(u8, u8, u8)> {
    let idx = match c {
        Color::Rgb(r, g, b) => return Some((r, g, b)),
        Color::Indexed(i) => return Some(ansi256_to_rgb(i)),
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Reset => return None,
    };
    Some(ansi256_to_rgb(idx))
}

/// Parse a concrete color literal: `indexed:N`, `rgb:r,g,b`, or a named color.
fn concrete(s: &str) -> Color {
    if let Some(n) = s.strip_prefix("indexed:") {
        return Color::Indexed(n.trim().parse().unwrap_or(0));
    }
    if let Some(rest) = s.strip_prefix("rgb:") {
        let mut it = rest.split(',').map(|p| p.trim().parse::<u8>().unwrap_or(0));
        return Color::Rgb(
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        );
    }
    match s {
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "dark_grey" => Color::DarkGray,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        _ => Color::Reset,
    }
}

/// The known leaf fields of a role table. Any other key whose value is a table
/// is treated as a state variant (`[role.<id>.<state>]`).
const ROLE_FIELDS: &[&str] = &[
    "extends", "fg", "bg", "glyph", "lead", "trail", "title", "bold", "dim", "italic",
    "underline", "reversed",
];

/// Overlay two state-variant deltas: fields set in `over` win over `base`.
fn raw_overlay(base: &RawStyle, over: &RawStyle) -> RawStyle {
    RawStyle {
        extends: None,
        fg: over.fg.clone().or_else(|| base.fg.clone()),
        bg: over.bg.clone().or_else(|| base.bg.clone()),
        glyph: over.glyph.clone().or_else(|| base.glyph.clone()),
        lead: None,
        trail: None,
        title: None,
        bold: over.bold.or(base.bold),
        dim: over.dim.or(base.dim),
        italic: over.italic.or(base.italic),
        underline: over.underline.or(base.underline),
        reversed: over.reversed.or(base.reversed),
    }
}

/// Collect `name`'s state-variant deltas, inheriting through `extends` (a
/// child's own delta for a state overlays the parent's). Cycle-guarded.
fn merged_variants(
    name: &str,
    role_raws: &HashMap<String, RawStyle>,
    variant_raws: &HashMap<String, HashMap<String, RawStyle>>,
    stack: &mut Vec<String>,
) -> HashMap<String, RawStyle> {
    if stack.iter().any(|n| n == name) {
        return HashMap::new();
    }
    let mut merged = match role_raws.get(name).and_then(|r| r.extends.clone()) {
        Some(ext) => {
            stack.push(name.to_string());
            let parent = merged_variants(&ext, role_raws, variant_raws, stack);
            stack.pop();
            parent
        }
        None => HashMap::new(),
    };
    if let Some(own) = variant_raws.get(name) {
        for (state, delta) in own {
            merged
                .entry(state.clone())
                .and_modify(|acc| *acc = raw_overlay(acc, delta))
                .or_insert_with(|| delta.clone());
        }
    }
    merged
}

/// Overlay one `RawStyle` onto a resolved parent, producing a `RoleStyle`.
fn role_style_from(
    palette: &HashMap<String, PaletteColor>,
    parent: &RoleStyle,
    raw: &RawStyle,
) -> RoleStyle {
    let mut rs = parent.clone();
    if let Some(c) = &raw.fg {
        rs.fg = spec(palette, c);
    }
    if let Some(c) = &raw.bg {
        rs.bg = spec(palette, c);
    }
    if raw.glyph.is_some() {
        rs.glyph = raw.glyph.clone();
    }
    if raw.lead.is_some() {
        rs.lead = raw.lead.clone();
    }
    if raw.trail.is_some() {
        rs.trail = raw.trail.clone();
    }
    rs.mods = rs.mods.overlay(Mods::from_raw(raw));
    rs
}

/// Resolve `name`'s full cascade into `out`, following `extends` first.
fn resolve_role(
    name: &str,
    raws: &HashMap<String, RawStyle>,
    base: &RoleStyle,
    palette: &HashMap<String, PaletteColor>,
    out: &mut HashMap<String, RoleStyle>,
    stack: &mut Vec<String>,
) -> RoleStyle {
    if let Some(rs) = out.get(name) {
        return rs.clone();
    }
    let Some(raw) = raws.get(name) else {
        return base.clone();
    };
    // Guard against an `extends` cycle by falling back to `base`.
    if stack.iter().any(|n| n == name) {
        return base.clone();
    }
    stack.push(name.to_string());
    let parent = match &raw.extends {
        Some(ext) => resolve_role(ext, raws, base, palette, out, stack),
        None => base.clone(),
    };
    stack.pop();
    let rs = role_style_from(palette, &parent, raw);
    out.insert(name.to_string(), rs.clone());
    rs
}

/// Build a resolved state node from a `RawStyle` (unset colors stay `None`).
fn state_node(palette: &HashMap<String, PaletteColor>, raw: &RawStyle) -> StateNode {
    StateNode {
        fg: raw.fg.as_ref().map(|c| spec(palette, c)),
        bg: raw.bg.as_ref().map(|c| spec(palette, c)),
        glyph: raw.glyph.clone(),
        mods: Mods::from_raw(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Ctx {
        Ctx { truecolor: true, background: Background::Dark }
    }

    #[test]
    fn embedded_theme_loads() {
        let _ = Theme::load();
    }

    #[test]
    fn nerd_overlay_replaces_only_glyphs() {
        let base = Theme::load();
        let nerd = Theme::load_with(GlyphSet::Nerd);
        // The overlay swaps the dirty marker glyph...
        assert_ne!(base.glyph("dirty"), nerd.glyph("dirty"));
        assert_eq!(nerd.glyph("dirty"), "\u{f0deb}");
        // ...and commit_worktree inherits it through `extends` (glyph changes,
        // its italic + warn color are untouched by the overlay).
        assert_eq!(nerd.glyph("commit_worktree"), "\u{f0deb}");
        let base_style = base.style("dirty", dark(), RainbowInput::None);
        let nerd_style = nerd.style("dirty", dark(), RainbowInput::None);
        assert_eq!(base_style.fg, nerd_style.fg);
        // Chips keep their semantic color; only the glyph differs.
        let (base_glyph, base_chip) = base.chip(ChipKind::Clean, dark());
        let (nerd_glyph, nerd_chip) = nerd.chip(ChipKind::Clean, dark());
        assert_ne!(base_glyph, nerd_glyph);
        assert_eq!(nerd_glyph, "\u{f00c}");
        assert_eq!(base_chip.fg, nerd_chip.fg);
    }

    #[test]
    fn glyph_set_from_str_defaults_to_unicode() {
        assert_eq!(GlyphSet::from_str("nerd"), GlyphSet::Nerd);
        assert_eq!(GlyphSet::from_str("NERD"), GlyphSet::Nerd);
        assert_eq!(GlyphSet::from_str("unicode"), GlyphSet::Unicode);
        assert_eq!(GlyphSet::from_str("whatever"), GlyphSet::Unicode);
    }

    #[test]
    fn every_role_the_renderer_uses_exists() {
        let t = Theme::load();
        for role in [
            "column_border",
            "divider",
            "divider_active",
            "row_hover",
            "column_title",
            "secondary",
            "row_text",
            "stack_name",
            "stack_counters",
            "stack_staircase",
            "stack_dirty",
            "commit_header",
            "segment_riser",
            "commit_hash",
            "commit_subject",
            "commit_worktree",
            "file_message_glyph",
            "file_message_path",
            "file_name",
            "file_dir",
            "diff_added",
            "diff_deleted",
            "diff_placeholder",
            "checks_summary",
            "dirty",
            "ahead",
            "behind",
            "churn_added",
            "churn_deleted",
            "legend_label",
            "overlay_frame",
            "breadcrumb",
            "hint_key",
            "hint_label",
            "hint_separator",
            "help_heading",
            "help_key",
            "help_footer",
            "palette_prompt",
            "palette_cursor",
            "palette_key",
            "action_button",
            "run_rerun",
            "run_close",
            "tab",
            "tab_active",
            "tab_close",
            "tab_close_active",
            "tab_status_running",
            "tab_status_failed",
            "tab_status_ok",
            "tab_capture",
            "run_header",
            "run_output",
        ] {
            assert!(t.roles.contains_key(role), "missing role {role}");
        }
    }

    #[test]
    fn palette_tokens_resolve_to_semantic_colors() {
        let t = Theme::load();
        // file_status `added` = palette.ok = an explicit green (truecolor RGB with
        // a 256-color fallback), pinned so it never picks up a palette's olive
        // "green" (ANSI 2).
        assert_eq!(t.file_status_style('A', dark()).fg, Some(Color::Rgb(63, 185, 80)));
        let idx = Ctx { truecolor: false, background: Background::Dark };
        assert_eq!(t.file_status_style('A', idx).fg, Some(Color::Indexed(40)));
        assert_eq!(t.file_status_style('D', dark()).fg, Some(Color::Red));
        assert_eq!(t.file_status_style('M', dark()).fg, Some(Color::Yellow));
        assert_eq!(t.file_status_style('R', dark()).fg, Some(Color::Cyan));
    }

    #[test]
    fn chips_are_semantic_not_rainbow() {
        let t = Theme::load();
        let (glyph, style) = t.chip(ChipKind::Clean, dark());
        assert_eq!(glyph, "✓");
        assert_eq!(style.fg, Some(Color::Rgb(63, 185, 80)));
        let (glyph, style) = t.chip(ChipKind::Error, dark());
        assert_eq!(glyph, "✗");
        assert_eq!(style.fg, Some(Color::Red));
    }

    #[test]
    fn extends_flattens_and_glyph_is_inherited() {
        let t = Theme::load();
        // commit_worktree extends dirty: inherits ✎ + warn, adds italic.
        assert_eq!(t.glyph("commit_worktree"), "✎");
        let style = t.style("commit_worktree", dark(), RainbowInput::None);
        assert_eq!(style.fg, Some(Color::Yellow));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn secondary_extenders_are_dim() {
        let t = Theme::load();
        for role in ["stack_counters", "commit_header", "diff_placeholder", "legend_label"] {
            let style = t.style(role, dark(), RainbowInput::None);
            assert!(
                style.add_modifier.contains(Modifier::DIM),
                "{role} should be dim"
            );
        }
    }

    #[test]
    fn rainbow_fg_depends_on_identity_input() {
        let t = Theme::load();
        // Two different stack names yield different hues (thus colors).
        let a = t.style("stack_name", dark(), RainbowInput::Key("feat/a")).fg;
        let b = t.style("stack_name", dark(), RainbowInput::Key("feat/b")).fg;
        assert!(a.is_some() && b.is_some() && a != b);
    }

    #[test]
    fn diff_backgrounds_have_capability_fallback() {
        let t = Theme::load();
        let full = Ctx { truecolor: true, background: Background::Dark };
        let idx = Ctx { truecolor: false, background: Background::Dark };
        // Truecolor gets a subtle RGB tint; 256-color falls back to a neutral
        // dark index (the colored edge marker carries add/del there).
        assert_eq!(t.style("diff_added", full, RainbowInput::None).bg, Some(Color::Rgb(18, 34, 24)));
        assert_eq!(t.style("diff_added", idx, RainbowInput::None).bg, Some(Color::Indexed(236)));
        assert_eq!(t.style("diff_deleted", idx, RainbowInput::None).bg, Some(Color::Indexed(236)));
    }

    #[test]
    fn plain_row_text_class_is_shared_and_brightens_on_selection() {
        let t = Theme::load();
        // commit_subject and file_dir share the row_text class: muted by default.
        let subject = t.style("commit_subject", dark(), RainbowInput::None);
        let dir = t.style("file_dir", dark(), RainbowInput::None);
        assert_eq!(subject.fg, Some(Color::Gray));
        assert_eq!(dir.fg, Some(Color::Gray));
        // The [role.row_text.row_selected] variant is inherited by both and
        // brightens them to emphasis on the selected row.
        let subject_sel = t.style_state("commit_subject", "row_selected", dark(), RainbowInput::None);
        let dir_sel = t.style_state("file_dir", "row_selected", dark(), RainbowInput::None);
        assert_eq!(subject_sel.fg, Some(Color::White));
        assert_eq!(dir_sel.fg, Some(Color::White));
        // A role with no such variant is unaffected by style_state.
        let hash = t.style("commit_hash", dark(), RainbowInput::Position { index: 0, total: 3 });
        let hash_sel =
            t.style_state("commit_hash", "row_selected", dark(), RainbowInput::Position { index: 0, total: 3 });
        assert_eq!(hash.fg, hash_sel.fg);
    }

    #[test]
    fn overlay_chrome_is_themed() {
        let t = Theme::load();
        // Accent chrome (hint keys, palette prompt/keys, help headings) is cyan.
        let hint_key = t.style("hint_key", dark(), RainbowInput::None);
        assert_eq!(hint_key.fg, Some(Color::Cyan));
        assert!(hint_key.add_modifier.contains(Modifier::BOLD));
        assert_eq!(t.style("palette_key", dark(), RainbowInput::None).fg, Some(Color::Cyan));
        // Help keybinding chords stay yellow; popup frames are white + bold.
        assert_eq!(t.style("help_key", dark(), RainbowInput::None).fg, Some(Color::Yellow));
        let frame = t.style("overlay_frame", dark(), RainbowInput::None);
        assert_eq!(frame.fg, Some(Color::White));
        assert!(frame.add_modifier.contains(Modifier::BOLD));
        // Glyphs live in the theme, not the renderer.
        assert_eq!(t.glyph("hint_separator"), "·");
        assert_eq!(t.glyph("palette_prompt"), "› ");
        assert_eq!(t.glyph("palette_cursor"), "▏");
        assert_eq!(t.glyph("breadcrumb"), "▸");
    }

    #[test]
    fn selection_and_focus_come_from_state_layers() {
        let t = Theme::load();
        assert_eq!(t.selection_symbol(), "▶ ");
        // Focused column: the normal selection bar. Unfocused: it lightens
        // instead of the column dimming its content.
        assert_eq!(t.selection_style(true, dark()).bg, Some(Color::Indexed(238)));
        assert_eq!(t.selection_style(false, dark()).bg, Some(Color::Indexed(235)));
        // Focused title brightens.
        let focused = t.column_title_style(true, dark());
        assert_eq!(focused.fg, Some(Color::White));
        assert!(focused.add_modifier.contains(Modifier::BOLD));
    }
}
