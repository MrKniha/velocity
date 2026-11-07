//! "Who's using Velocity?": shown at start when there is more than one profile and none was named on the
//! command line. Picking a profile starts it in its own process (`--profile`) and closes this window.

use std::path::Path;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Icon, WindowBuilder};
use wry::{WebContext, WebViewBuilder};

use crate::{icon, media, profiles, profiles_js, settings::Settings, THEME_JS};

pub fn run(base: &Path) -> wry::Result<()> {
    let settings = Settings::load(base); // the Default profile's colours
    let (r, g, b) = settings.frame_rgb();
    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let logo = || Icon::from_rgba(icon::logo_rgba(64), 64, 64).ok();
    let window = WindowBuilder::new()
        .with_title("Velocity")
        .with_window_icon(logo())
        .with_taskbar_icon(logo())
        .with_inner_size(tao::dpi::LogicalSize::new(880.0, 600.0))
        .with_background_color((r, g, b, 255))
        .build(&event_loop)
        .expect("window");
    crate::dark_frame(&window);

    // Its own small WebView2 folder, so it never holds a lock on a profile it is about to start.
    let mut ctx = WebContext::new(Some(base.join("picker")));
    let proxy = event_loop.create_proxy();
    let init = format!("{THEME_JS}window.VELOCITY={};window.PROFILES={};", settings.to_js(), profiles_js(base));
    let view = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(include_str!("pages/picker.html"))
        .with_initialization_script(init)
        .with_background_color((r, g, b, 255))
        .with_navigation_handler(|url| !url.starts_with("http")) // only ever our own page
        .with_ipc_handler(move |req| {
            let _ = proxy.send_event(req.body().clone());
        })
        .build(&window)?;
    media::map(&view, profiles::AVATAR_HOST, &profiles::avatars_dir(base));

    let base = base.to_path_buf();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(msg) => {
                let (cmd, name) = msg.split_once(':').unwrap_or((&msg, ""));
                let chosen = match cmd {
                    "open" => profiles::list(&base).into_iter().find(|n| n == name),
                    "new" => profiles::create(&base, name),
                    _ => None,
                };
                match chosen {
                    Some(n) => {
                        profiles::launch(&n);
                        *control_flow = ControlFlow::Exit;
                    }
                    None if cmd == "new" => {
                        let _ = view.evaluate_script("addError('That name is taken or not allowed.')");
                    }
                    None => {}
                }
            }
            _ => {}
        }
    });
}
