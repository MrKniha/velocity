//! "Who's using Velocity?": shown at start when there is more than one profile and none was named on the
//! command line. Picking a profile starts it in its own process (`--profile`) and closes this window.
//! The page runs on Velocity's engine, as the tab UI does (`Engine::open_ui`).

use std::path::Path;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Icon, WindowBuilder};

use crate::{engine::Engine, icon, profiles, profiles_js, settings::Settings, UserEvent, THEME_JS};

/// `url`: the link Velocity was started with, opened in whichever profile is picked.
pub fn run(base: &Path, url: Option<String>) {
    let settings = Settings::load(base); // the Default profile's colours
    let (r, g, b) = settings.frame_rgb();
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
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

    // Its own small engine folder, so it never holds a lock on a profile it is about to start; no ad rules.
    let data = base.join("picker");
    let engine = Engine::new(&window, &event_loop, event_loop.create_proxy(), &data.join("engine"), &data.join("no-rules"));
    let init = format!("window.VELOCITY={};window.PROFILES={};", settings.to_js(), profiles_js(base));
    let files = vec![(profiles::AVATAR_HOST.to_string(), profiles::avatars_dir(base))];
    engine.open_ui(&window, &event_loop, include_str!("pages/picker.html"), &[THEME_JS, &init], files);
    let fill = |engine: &Engine, window: &tao::window::Window| {
        let size = window.inner_size();
        engine.ui_bounds(0, 0, size.width, size.height);
        engine.ui_visible(true);
    };
    fill(&engine, &window);

    let base = base.to_path_buf();
    let ui_window = engine.ui_window_id();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Engine) => engine.spin(),
            Event::RedrawRequested(id) if Some(id) == ui_window => engine.paint_ui(),
            Event::WindowEvent { window_id, event, .. } if Some(window_id) == ui_window => drop(engine.handle_ui(&event)),
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => fill(&engine, &window),
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Ui(msg)) => {
                let (cmd, name) = msg.split_once(':').unwrap_or((&msg, ""));
                let chosen = match cmd {
                    "open" => profiles::list(&base).into_iter().find(|n| n == name),
                    "new" => profiles::create(&base, name),
                    _ => None,
                };
                match chosen {
                    Some(n) => {
                        profiles::launch(&n, url.as_deref());
                        *control_flow = ControlFlow::Exit;
                    }
                    None if cmd == "new" => engine.ui_js("addError('That name is taken or not allowed.')"),
                    None => {}
                }
            }
            _ => {}
        }
    });
}
