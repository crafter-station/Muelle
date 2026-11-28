//! What a theme looks like once Windows' own preferences are taken into
//! account: shared by everything Muelle draws.

use crate::Appearance;

/// What the glass is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    /// The dock as it always was: the light or dark macOS glass.
    Standard,
    Frosted,
    Clear,
    LiquidGlass,
}

impl Material {
    pub const ALL: [Self; 4] = [
        Self::Standard,
        Self::Frosted,
        Self::Clear,
        Self::LiquidGlass,
    ];
}

/// What a surface is drawn for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub material: Material,
    /// Light surfaces with dark content; otherwise the reverse.
    pub light: bool,
    /// See `system::transparency_enabled`: without it every material is solid.
    pub glass: bool,
}

#[must_use]
pub fn resolve(choice: Appearance, system_light: bool, glass: bool) -> Look {
    let (material, light) = match choice {
        Appearance::Auto => (Material::Standard, system_light),
        Appearance::Light => (Material::Standard, true),
        Appearance::Dark => (Material::Standard, false),
        Appearance::Frosted => (Material::Frosted, system_light),
        Appearance::Clear => (Material::Clear, false),
        Appearance::LiquidGlass => (Material::LiquidGlass, system_light),
    };
    Look {
        material,
        light,
        glass,
    }
}
