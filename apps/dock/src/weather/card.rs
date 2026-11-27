use std::sync::OnceLock;
use anyhow::Result;
use windows::{
    UI::{Color, Composition::{CompositionSurfaceBrush, ContainerVisual, Desktop::DesktopWindowTarget}},
    Win32::{Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE}, WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, LoadCursorW, RegisterClassW,
            SetCursor, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, GWLP_USERDATA,
            HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SW_SHOWNOACTIVATE, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSELEAVE, WM_MOUSEMOVE,
            WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        }}},
    core::{HSTRING, Interface, w},
};
use windows_numerics::{Vector2, Vector3};
use crate::{appearance::Look, gfx::{Gfx, Ink, LabelBrush, WeatherGlyph}, scene::Theme};
use super::Placement;

#[derive(Clone, Copy, PartialEq)]
pub struct Key { pub placement: Placement, pub look: Look, pub scale: f32, pub revision: u64, pub language: muelle_config::Language }

pub struct Model { pub degrees: String, pub title: String, pub subtitle: String, pub glyph: WeatherGlyph }

pub struct Card { hwnd: HWND, target: DesktopWindowTarget, pub key: Key }

impl Card {
    pub fn new(gfx: &Gfx, key: Key, model: &Model) -> Result<Self> {
        let root = surface(gfx,key,model)?;
        let hwnd = create(key.placement)?;
        let target = unsafe { gfx.compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd,true) };
        let target = match target { Ok(target) => target, Err(error) => { unsafe { let _=DestroyWindow(hwnd); } return Err(error.into()); } };
        let view = Self { hwnd,target,key };
        view.target.SetRoot(&root)?;
        view.caption(model)?;
        unsafe { let _=ShowWindow(hwnd,SW_SHOWNOACTIVATE); }
        Ok(view)
    }

    pub fn update(&mut self,gfx:&Gfx,key:Key,model:&Model)->Result<()> {
        let root=surface(gfx,key,model)?;
        self.target.SetRoot(&root)?;
        let r=key.placement.bounds;
        if self.key.placement!=key.placement {
            unsafe { SetWindowPos(self.hwnd,Some(HWND_TOPMOST),r.left,r.top,r.right-r.left,r.bottom-r.top,SWP_NOACTIVATE)?; }
        }
        self.caption(model)?;
        self.key=key;
        Ok(())
    }
    fn caption(&self,model:&Model)->Result<()> {
        unsafe { SetWindowTextW(self.hwnd,&HSTRING::from(format!("{} · {} · {}",model.degrees,model.title,model.subtitle)))?; }
        Ok(())
    }
    pub fn hwnd(&self)->HWND { self.hwnd }
    pub fn raise(&self) {
        // The Dock's larger invisible magnification window must not sit over
        // this card's input. Only called with existing Dock placement work.
        unsafe { let _=SetWindowPos(self.hwnd,Some(HWND_TOPMOST),0,0,0,0,SWP_NOACTIVATE|SWP_NOMOVE|SWP_NOSIZE); }
    }
    pub fn refresh_backdrops(&self,gfx:&Gfx)->Result<()> {
        if let Ok(root)=self.target.Root() { gfx.refresh_backdrops(&root.cast()?)?; }
        Ok(())
    }
}

impl Drop for Card { fn drop(&mut self) { unsafe { let _=DestroyWindow(self.hwnd); } } }

fn add(gfx:&Gfx,root:&ContainerVisual,brush:&CompositionSurfaceBrush,x:f32,y:f32,w:f32,h:f32)->Result<()> {
    let sprite=gfx.compositor.CreateSpriteVisual()?;
    sprite.SetBrush(brush)?;sprite.SetSize(Vector2::new(w,h))?;sprite.SetOffset(Vector3::new(x,y,0.0))?;
    root.Children()?.InsertAtTop(&sprite)?; Ok(())
}
fn label(gfx:&Gfx,root:&ContainerVisual,brush:LabelBrush,x:f32,y:f32,opacity:f32)->Result<()> {
    let sprite=gfx.compositor.CreateSpriteVisual()?;
    sprite.SetBrush(&brush.brush)?;sprite.SetSize(Vector2::new(brush.width,brush.height))?;
    sprite.SetOffset(Vector3::new(x,y,0.0))?;sprite.SetOpacity(opacity)?;
    root.Children()?.InsertAtTop(&sprite)?;Ok(())
}

fn surface(gfx:&Gfx,key:Key,model:&Model)->Result<ContainerVisual> {
    let r=key.placement.bounds;
    let (width,height)=((r.right-r.left) as f32,(r.bottom-r.top) as f32);
    let s=key.scale;
    let root=Theme::for_popup_look(key.look).panel(gfx,Vector2::new(width,height),18.0*s,s)?;
    let round=gfx.compositor.CreateRoundedRectangleGeometry()?;
    round.SetSize(Vector2::new(width,height))?;round.SetCornerRadius(Vector2::new(18.0*s,18.0*s))?;
    root.SetClip(&gfx.compositor.CreateGeometricClipWithGeometry(&round)?)?;
    let ink=if key.look.light {Ink::Dark}else{Ink::Light};
    // A small static sky tint; it never replaces the selected material or
    // introduces another backdrop, texture capture or animated gradient.
    let tint=gfx.compositor.CreateLinearGradientBrush()?;
    tint.SetStartPoint(Vector2::new(0.0,0.0))?;tint.SetEndPoint(Vector2::new(1.0,1.0))?;
    let colors=tint.ColorStops()?;
    for (at,a) in [(0.0,34),(1.0,0)] { colors.Append(&gfx.compositor.CreateColorGradientStopWithOffsetAndColor(at,Color{A:a,R:72,G:156,B:244})?)?; }
    let sky=gfx.compositor.CreateSpriteVisual()?;sky.SetSize(Vector2::new(width,height))?;sky.SetBrush(&tint)?;
    // Clip the accent too, not just the material's own backdrop.
    root.Children()?.InsertAtTop(&sky)?;
    let icon=gfx.weather_icon(model.glyph,s)?;
    let icon_x=width-44.0*s;
    add(gfx,&root,&icon,icon_x,(height-40.0*s)/2.0,40.0*s,40.0*s)?;
    let degrees_x=if key.placement.compact {10.0*s}else{width-104.0*s};
    let degrees=gfx.text_brush_fitted(&model.degrees,s,28.0,false,ink,(icon_x-degrees_x-2.0*s).max(1.0))?;
    let degrees_y=(height-degrees.height)/2.0;
    label(gfx,&root,degrees,degrees_x,degrees_y,1.0)?;
    if !key.placement.compact {
        let text_width=(degrees_x-21.0*s).max(1.0);
        label(gfx,&root,gfx.text_brush_fitted(&model.title,s,12.0,true,ink,text_width)?,13.0*s,10.0*s,1.0)?;
        label(gfx,&root,gfx.text_brush_fitted(&model.subtitle,s,10.0,false,ink,text_width)?,13.0*s,31.0*s,0.67)?;
    }
    Ok(root)
}

fn create(place:Placement)->Result<HWND> {
    static CLASS:OnceLock<()>=OnceLock::new();
    unsafe {
        let instance=GetModuleHandleW(None)?;
        let class=w!("MuelleWeather");
        CLASS.get_or_init(||{RegisterClassW(&WNDCLASSW{lpfnWndProc:Some(procedure),hInstance:instance.into(),lpszClassName:class,..Default::default()});});
        let r=place.bounds;
        let hwnd=CreateWindowExW(WS_EX_NOACTIVATE|WS_EX_NOREDIRECTIONBITMAP|WS_EX_TOOLWINDOW|WS_EX_TOPMOST,
            class,w!("Weather"),WS_POPUP,r.left,r.top,r.right-r.left,r.bottom-r.top,None,None,Some(instance.into()),None)?;
        crate::menu::apply_popup_frame(hwnd);
        Ok(hwnd)
    }
}

unsafe extern "system" fn procedure(hwnd:HWND,msg:u32,w:WPARAM,l:LPARAM)->LRESULT {
    match msg {
        WM_MOUSEACTIVATE=>return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE=>unsafe {
            if GetWindowLongPtrW(hwnd,GWLP_USERDATA)==0 {
                let mut tracked=TRACKMOUSEEVENT{cbSize:size_of::<TRACKMOUSEEVENT>() as u32,dwFlags:TME_LEAVE,hwndTrack:hwnd,dwHoverTime:0};
                if TrackMouseEvent(&mut tracked).is_ok() {
                    SetWindowLongPtrW(hwnd,GWLP_USERDATA,1);
                    crate::app::weather_signal(hwnd,1);
                }
            }
        },
        WM_MOUSELEAVE=>unsafe { SetWindowLongPtrW(hwnd,GWLP_USERDATA,0);crate::app::weather_signal(hwnd,0); },
        WM_LBUTTONUP|WM_RBUTTONUP=>crate::app::weather_signal(hwnd,2),
        WM_SETCURSOR=>{
            unsafe { if let Ok(cursor)=LoadCursorW(None,IDC_ARROW){SetCursor(Some(cursor));} }
            return LRESULT(1);
        }
        _=>{}
    }
    unsafe {DefWindowProcW(hwnd,msg,w,l)}
}
