//! Opening groups and coordinating their detached input/drag windows.

use anyhow::Result;
use muelle_core::magnify::magnify;
use windows::Win32::{
    Foundation::POINT,
    UI::{Input::KeyboardAndMouse::SetCapture, WindowsAndMessaging::SetTimer},
};

use crate::{
    assets::fallback_name,
    group_view::{DragGhost, GroupItem, GroupView, Placement, Released},
    menu,
    scene::Theme,
    strings::text,
};

/// Files shown in a Downloads stack, newest first; the folder itself comes
/// after them. Past a fan's worth they fold into a grid, as in macOS.
const STACK_FILES: usize = 24;

use super::{
    App, DRAG_POLL_MS, DRAG_TIMER, Dragging, HIDE_DELAY_MS, HIDE_TIMER, MenuRequest, MenuTarget,
    Outside, Role,
};

impl App {
    /// Unfolds the group `id` beside its icon, away from the screen edge.
    pub(super) fn open_group_view(&mut self, id: &str) -> Result<()> {
        self.close_preview();
        self.close_group_view();
        let Some(stage) = &self.stage else {
            return Ok(());
        };
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.id.as_deref() == Some(id))
        else {
            return Ok(());
        };
        let state = magnify(
            &stage.layout,
            self.cursor,
            self.amount.value,
            &stage.magnifier,
        );
        let Some(rect) = state.rects.get(index) else {
            return Ok(());
        };
        let anchor = stage.screen_rect(*rect);
        let (icon, scale) = (stage.icon.round(), stage.scale);
        let theme = Theme::new(stage.key.look);
        // The dock's own display, which a hidden dock can move between.
        let (screen, edge) = (stage.key.screen, stage.frame.edge);
        let items = if self.is_downloads_stack(id) {
            self.stack_items(id, icon as u32)
        } else {
            self.group_items(id, icon as u32)
        };
        self.group_view = Some(GroupView::show(
            &self.gfx,
            id.to_owned(),
            &items,
            &Placement {
                anchor,
                screen,
                edge,
                scale,
                icon,
                theme,
                dock: self.input,
            },
        )?);
        self.sync_badge_visuals()?;
        self.render()
    }

    /// The apps of the group `id`, drawn `side` pixels wide.
    pub(super) fn group_items(&mut self, id: &str, side: u32) -> Vec<GroupItem> {
        let apps = self
            .roster
            .group(id)
            .map(|group| group.apps.clone())
            .unwrap_or_default();
        let mut items = Vec::with_capacity(apps.len());
        for app in apps {
            let visual = self.icons.get(&app, "");
            let icon = visual
                .and_then(|visual| {
                    self.gfx
                        .icon_brush(&visual.icon, side.min(visual.icon.width))
                        .inspect_err(|error| eprintln!("muelle: icono de `{app}`: {error:#}"))
                        .ok()
                })
                .or_else(|| self.placeholder.clone());
            let name = visual.map_or_else(|| fallback_name(&app), |visual| visual.name.clone());
            items.push(GroupItem {
                running: self.roster.is_running(&app),
                id: app,
                name,
                icon,
            });
        }
        items
    }

    /// Whether `id` is the Downloads folder unfolded as a stack, not a group.
    pub(super) fn is_downloads_stack(&self, id: &str) -> bool {
        self.config.downloads_stack && self.downloads.as_deref() == Some(id)
    }

    /// The newest files of `folder`, drawn `side` pixels wide, then the
    /// folder itself, as a macOS stack ends with a way to Finder.
    pub(super) fn stack_items(&mut self, folder: &str, side: u32) -> Vec<GroupItem> {
        let files = muelle_shell::folder::newest(folder, STACK_FILES)
            .inspect_err(|error| eprintln!("muelle: Descargas: {error}"))
            .unwrap_or_default();
        let mut items = Vec::with_capacity(files.len() + 1);
        for file in files {
            let icon = muelle_shell::icons::load_preview(&file.path, side)
                .ok()
                .and_then(|image| self.gfx.icon_brush(&image, side.min(image.width)).ok())
                .or_else(|| self.placeholder.clone());
            items.push(GroupItem {
                id: file.path,
                name: file.name,
                icon,
                running: false,
            });
        }
        let icon = self
            .icons
            .get(folder, "")
            .and_then(|visual| {
                self.gfx
                    .icon_brush(&visual.icon, side.min(visual.icon.width))
                    .ok()
            })
            .or_else(|| self.placeholder.clone());
        items.push(GroupItem {
            id: folder.to_owned(),
            name: text().show_in_explorer.replace('&', ""),
            icon,
            running: false,
        });
        items
    }

    pub(super) fn close_group_view(&mut self) {
        if self.group_view.take().is_some()
            && self.config.hides_automatically()
            && !self.hovering
            && self.drag.is_none()
        {
            // SAFETY: Timer on our own window.
            unsafe { SetTimer(Some(self.input), HIDE_TIMER, HIDE_DELAY_MS, None) };
        }
    }

    /// The pointer moved over the unfolded group: past the drag threshold,
    /// the app pressed leaves the group.
    pub(super) fn group_view_moved(&mut self) -> Result<()> {
        // Files stay in Downloads: a stack has nothing to drag out of it.
        let stack = self
            .group_view
            .as_ref()
            .is_some_and(|view| self.is_downloads_stack(&view.group_id));
        let Some(view) = &mut self.group_view else {
            return Ok(());
        };
        if !stack && let Some((id, point)) = view.drag_started()? {
            return self.start_group_drag(id, point);
        }
        view.update()
    }

    /// An app dragged out of an unfolded group: it leaves the group for the
    /// dock, right after the group, and the drag goes on as a dock drag. So
    /// it can be placed among the icons or dropped on another group, and
    /// released anywhere else it simply stays out of the group.
    pub(super) fn start_group_drag(&mut self, id: String, point: POINT) -> Result<()> {
        // Closing the view also releases the mouse it held.
        self.close_group_view();
        if !self.roster.ungroup(&id) {
            return Ok(());
        }
        let side = self.stage.as_ref().map_or(48.0, |stage| {
            (stage.icon * stage.magnifier.max_scale).round()
        });
        let brush = self
            .icons
            .get(&id, "")
            .and_then(|visual| {
                self.gfx
                    .icon_brush(&visual.icon, (side as u32).min(visual.icon.width))
                    .ok()
            })
            .or_else(|| self.placeholder.clone());
        self.ghost = brush.and_then(|brush| {
            DragGhost::show(&self.gfx, &brush, side, point)
                .inspect_err(|error| eprintln!("muelle: icono arrastrado: {error:#}"))
                .ok()
        });
        self.press = None;
        self.drag = Some(Dragging {
            id,
            point,
            removing: false,
            merge: None,
            from_group: true,
        });
        self.refresh()?;
        if let Some(stage) = &self.stage {
            self.cursor = stage.to_dock(point).0;
        }
        // SAFETY: Capture keeps mouse input while dragging outside the dock.
        unsafe {
            SetCapture(self.input);
            SetTimer(Some(self.input), DRAG_TIMER, DRAG_POLL_MS, None);
        }
        self.drag_to(point)
    }

    pub(super) fn group_view_released(&mut self) -> Result<Option<Outside>> {
        let Some(view) = &mut self.group_view else {
            return Ok(None);
        };
        let group_id = view.group_id.clone();
        Ok(match view.release()? {
            Released::Item(id) => {
                self.close_group_view();
                if self.is_downloads_stack(&group_id) {
                    // A file opens in its app; the last item opens the folder.
                    Some(Outside::Open(id))
                } else {
                    self.click_app(id)
                }
            }
            Released::Dismiss => {
                self.close_group_view();
                None
            }
            Released::Nothing => None,
        })
    }

    /// Right-click on an app in an unfolded group: that app's dock menu.
    pub(super) fn group_view_menu(&mut self) -> Result<Option<Outside>> {
        let Some(view) = &self.group_view else {
            return Ok(None);
        };
        let hit = view.item_at_cursor()?;
        let stack = self.is_downloads_stack(&view.group_id);
        self.close_group_view();
        let Some((id, rect)) = hit else {
            return Ok(None);
        };
        // In a Downloads stack, right-click shows the file in Explorer.
        if stack {
            return Ok(Some(Outside::Reveal(id)));
        }
        let windows = self.windows_of(&id);
        let (items, end_task, jump) = self.entry_menu(&id, Role::App, &windows);
        let gap = self
            .stage
            .as_ref()
            .map_or(0, |stage| (stage.gap / 2.0).round() as i32);
        let anchor = POINT {
            x: (rect.left + rect.right) / 2,
            y: rect.top - gap,
        };
        self.render()?;
        Ok(Some(Outside::Menu(MenuRequest {
            gfx: self.gfx.clone(),
            target: MenuTarget::Entry {
                id,
                role: Role::App,
                windows,
                end_task,
                jump,
            },
            items,
            anchor,
            placement: menu::Placement::Above,
            dark: !crate::appearance::light(),
        })))
    }
}
