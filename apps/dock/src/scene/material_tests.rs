use super::*;

#[test]
fn bar_popups_preserve_explicit_materials_and_readable_solid_fallbacks() {
    for material in Material::ALL {
        for light in [false, true] {
            for glass in [false, true] {
                let look = Look {
                    material,
                    light,
                    glass,
                };
                let popup = Theme::for_bar_popup(look);
                let expected = if material == Material::Standard {
                    Theme::for_popup(light, glass)
                } else {
                    Theme::new(look)
                };
                assert_eq!(popup.light, light);
                assert_eq!(popup.glass, glass);
                assert_eq!(popup.panel_tint, expected.panel_tint);
                assert_eq!(popup.rim, expected.rim);
                assert_eq!(popup.sheen, expected.sheen);
                assert_eq!(popup.lens, material == Material::LiquidGlass && glass);
                if !glass {
                    assert_eq!(popup.panel_tint.A, 255);
                }
            }
        }
    }
}

#[test]
fn group_tiles_use_the_dock_material_and_stay_opaque_without_transparency() {
    for material in Material::ALL {
        for light in [false, true] {
            for glass in [false, true] {
                let theme = Theme::new(Look {
                    material,
                    light,
                    glass,
                });
                let group = theme.group_style();
                assert_eq!(group.fill, theme.tint);
                assert_eq!(group.sheen.A, theme.sheen);
                assert_eq!(group.rim_top.A, theme.rim[0]);
                assert_eq!(group.fill.A == 255, !glass);
                assert_eq!(group.ink.R < 100, light);
            }
        }
    }
    let palette = |material| {
        Theme::new(Look {
            material,
            light: false,
            glass: true,
        })
        .group_style()
    };
    assert_ne!(palette(Material::Frosted), palette(Material::Clear));
    assert_ne!(palette(Material::Frosted), palette(Material::LiquidGlass));
}

#[test]
#[ignore = "Offscreen compositor surfaces only; no desktop, input or settings changes"]
fn native_group_tiles_draw_themed_icons_loading_cells_and_overflow_at_each_scale() -> Result<()> {
    use windows::Win32::System::WinRT::{
        CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
        DispatcherQueueOptions,
    };
    let _queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?
    };
    let gfx = Gfx::new()?;
    let icon = IconImage {
        width: 2,
        height: 2,
        bgra: [80, 130, 190, 255].repeat(4),
    };
    for material in Material::ALL {
        for light in [false, true] {
            for glass in [false, true] {
                let style = Theme::new(Look {
                    material,
                    light,
                    glass,
                })
                .group_style();
                for side in [32, 48, 96, 144] {
                    for (icons, count) in [
                        ([None; 4], 1),
                        ([Some(&icon); 4], 4),
                        ([Some(&icon); 4], 12),
                    ] {
                        let brush = gfx.group_brush(side, &icons, count, style)?;
                        assert!(brush.Surface().is_ok());
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn frosted_keeps_the_backdrop_visible_and_its_text_panels_denser() {
    for light in [false, true] {
        let look = Look {
            material: Material::Frosted,
            light,
            glass: true,
        };
        let dock = Theme::new(look);
        let bar = Theme::for_bar(look);
        // The icon surface retains at least 40% of its backdrop. The text
        // strip can be denser, but must still retain at least a quarter.
        assert!(255 - u16::from(dock.tint.A) >= 102);
        assert!(255 - u16::from(bar.tint.A) >= 64);
        assert!(dock.panel_tint.A > dock.tint.A);
        assert_eq!(dock.light, light);
    }
    let light = Theme::new(Look {
        material: Material::Frosted,
        light: true,
        glass: true,
    });
    let dark = Theme::new(Look {
        material: Material::Frosted,
        light: false,
        glass: true,
    });
    assert!(light.tint.R > dark.tint.R && light.tint.G > dark.tint.G && light.tint.B > dark.tint.B);
}

#[test]
fn disabling_transparency_keeps_every_palette_opaque_including_clear() {
    for material in Material::ALL {
        for light in [false, true] {
            let look = Look {
                material,
                light,
                glass: false,
            };
            let dock = Theme::new(look);
            let bar = Theme::for_bar(look);
            assert!(!dock.glass && !bar.glass);
            assert_eq!(dock.tint.A, 255);
            assert_eq!(dock.panel_tint.A, 255);
            assert_eq!(bar.tint.A, 255);
            assert!(!dock.lens && !bar.lens);
        }
    }
}

#[test]
#[ignore = "Native composition regression; no windows, wallpaper or system preference changes"]
fn glass_materials_build_bounded_backdrop_layers_only_when_windows_allows_them() -> Result<()> {
    use windows::{
        UI::Composition::{
            CompositionBackdropBrush, CompositionColorBrush, CompositionLinearGradientBrush,
        },
        Win32::System::WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        },
    };
    // SAFETY: Only this thread's private dispatcher/compositor is created.
    let _queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?
    };
    let gfx = Gfx::new()?;
    for material in [Material::Frosted, Material::Clear, Material::LiquidGlass] {
        for light in [false, true] {
            for glass in [false, true] {
                let look = Look {
                    material,
                    light,
                    glass,
                };
                for theme in [
                    Theme::new(look),
                    Theme::for_bar(look),
                    Theme::for_bar_popup(look),
                ] {
                    let fills = theme.fills(&gfx)?;
                    let backdrops = fills
                        .iter()
                        .filter(|brush| brush.cast::<CompositionBackdropBrush>().is_ok())
                        .count();
                    assert_eq!(backdrops, usize::from(glass));
                    let tint = fills[usize::from(glass)]
                        .cast::<CompositionColorBrush>()?
                        .Color()?;
                    assert_eq!(tint, theme.tint);
                    assert_eq!(tint.A == 255, !glass);
                    let lens = material == Material::LiquidGlass && glass;
                    assert_eq!(fills.len(), 1 + usize::from(glass) + usize::from(lens));
                    if lens {
                        let coating = fills[2].cast::<CompositionLinearGradientBrush>()?;
                        assert_eq!(coating.ColorStops()?.Size()? as usize, LENS_COATING.len());
                        assert_eq!(coating.EndPoint()?, v2(0.0, 1.0));
                        for (edge, start, end) in [
                            (DockEdge::Left, v2(1.0, 0.0), v2(0.0, 0.0)),
                            (DockEdge::Right, v2(0.0, 0.0), v2(1.0, 0.0)),
                        ] {
                            let fills = theme.fills_on(&gfx, edge)?;
                            let coating = fills[2].cast::<CompositionLinearGradientBrush>()?;
                            assert_eq!((coating.StartPoint()?, coating.EndPoint()?), (start, end));
                        }
                    }
                    // Build the real clipped surface as well as its brushes. This
                    // catches unsupported compositor calls without touching the desktop.
                    let panel = theme.panel(&gfx, v2(320.0, 160.0), 20.0, 1.0)?;
                    assert_eq!(panel.Children()?.Count()?, 2);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn liquid_glass_has_a_quieter_centre_and_readable_panels_in_both_palettes() {
    for light in [false, true] {
        let look = Look {
            material: Material::LiquidGlass,
            light,
            glass: true,
        };
        let liquid = Theme::new(look);
        let frosted = Theme::new(Look {
            material: Material::Frosted,
            ..look
        });
        assert!(liquid.tint.A < frosted.tint.A);
        assert!(liquid.rim[0] > frosted.rim[0]);
        assert!(liquid.panel_tint.A >= 200);
        assert!(liquid.lens && Theme::for_bar(look).lens);
        assert!(!frosted.lens);
    }
}
