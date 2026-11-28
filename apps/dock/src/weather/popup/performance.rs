//! Manual release measurement on hidden owned windows. Never runs in normal CI.
use super::*;
use crate::appearance::Material;
use std::{hint::black_box, time::Instant};
use windows::Win32::{
    Foundation::FILETIME,
    System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
        Threading::{
            GR_USEROBJECTS, GetCurrentProcess, GetCurrentThread, GetGuiResources, GetThreadTimes,
        },
        WinRT::{
            CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            DispatcherQueueOptions,
        },
    },
};
fn private_bytes() -> Result<usize> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast(), counters.cb)?;
    }
    Ok(counters.PrivateUsage)
}
fn thread_time() -> Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )?;
    }
    let ticks = |v: FILETIME| (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime);
    Ok(ticks(kernel) + ticks(user))
}
fn idle() -> Result<u64> {
    // Block in the same native message loop used by the UI. A single harness
    // timer bounds the sample; sleeping would hide any accidental UI polling.
    let before = thread_time()?;
    let timer = unsafe { SetTimer(None, 0, 1000, None) };
    anyhow::ensure!(timer != 0, "cannot schedule measurement boundary");
    let mut message = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if got.0 <= 0 {
            unsafe {
                let _ = KillTimer(None, timer);
            }
            if got.0 == 0 {
                unsafe {
                    PostQuitMessage(message.wParam.0 as i32);
                }
            }
            anyhow::bail!("measurement message loop ended unexpectedly");
        }
        if message.hwnd == HWND::default()
            && message.message == WM_TIMER
            && message.wParam.0 == timer
        {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    unsafe {
        let _ = KillTimer(None, timer);
    }
    Ok(thread_time()?.saturating_sub(before) / 10)
}

fn idle_samples() -> Result<[u64; 3]> {
    Ok([idle()?, idle()?, idle()?])
}
#[test]
#[ignore = "Release-only performance sample: hidden HWNDs, no network/location/focus or live app changes"]
fn measure_weather_retained_ui_in_release() -> Result<()> {
    anyhow::ensure!(
        !cfg!(debug_assertions),
        "run with --release, not a debug build"
    );
    let _queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?
    };
    let gfx = Gfx::new()?;
    let mut forecast = muelle_weather::decode(include_bytes!(
        "../../../../../crates/muelle-weather/tests/forecast.json"
    ))?;
    let start = forecast.days[0].time;
    forecast.hours = (1..24)
        .map(|i| muelle_weather::Hour {
            time: forecast.current.time + i * 3600,
            temperature_c: 15.0 + (i % 7) as f32,
            feels_like_c: Some(14.0),
            wind_kmh: Some(9.0),
            condition: muelle_weather::Condition::PartlyCloudy,
            precipitation_percent: Some((i * 3) as u8),
            is_day: Some(i < 8),
        })
        .collect();
    forecast.days = (0..5)
        .map(|i| muelle_weather::Day {
            time: start + i * 86400,
            low_c: 10.0 + i as f32,
            high_c: 19.0 + i as f32,
            condition: muelle_weather::Condition::PartlyCloudy,
            wind_max_kmh: Some(18.0),
            precipitation_percent: Some(30),
            sunrise: Some(start + i * 86400 + 6 * 3600),
            sunset: Some(start + i * 86400 + 18 * 3600),
        })
        .collect();
    let data = Data::new(
        Some(&forecast),
        Language::Es,
        forecast.current.time,
        data::Status {
            has_location: true,
            ..Default::default()
        },
    );
    let context = Context {
        anchor: RECT {
            left: -32000,
            top: -31000,
            right: -31780,
            bottom: -30952,
        },
        screen: RECT {
            left: -33000,
            top: -33000,
            right: -30000,
            bottom: -30000,
        },
        edge: DockEdge::Bottom,
        scale: 1.0,
        look: Look {
            material: Material::Frosted,
            light: false,
            glass: true,
        },
        language: Language::Es,
        revision: 1,
        hour: 0,
        has_data: true,
    };
    // Warm DirectWrite and driver caches before comparing resident resources.
    drop(Popup::new(&gfx, context, &data)?);
    let idle_off_us = idle_samples()?;
    let baseline_user = unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
    let baseline_private = private_bytes()?;
    let card_model = crate::weather::Model {
        degrees: "17°".into(),
        title: "Parcialmente nublado".into(),
        subtitle: "Ubicación actual".into(),
        glyph: crate::gfx::WeatherGlyph::PartlyCloudy,
    };
    let card = crate::weather::Card::new(
        &gfx,
        crate::weather::Key {
            placement: crate::weather::Placement {
                bounds: context.anchor,
                compact: false,
            },
            screen: context.screen,
            look: context.look,
            scale: context.scale,
            language: context.language,
            revision: 0,
        },
        &card_model,
    )?;
    let idle_card_us = idle_samples()?;
    let card_private = private_bytes()?;
    let mut panel = Popup::new(&gfx, context, &data)?;
    let idle_panel_us = idle_samples()?;
    let open_private = private_bytes()?;
    let root = panel.target.Root()?;
    let start = Instant::now();
    for _ in 0..10000 {
        panel.update(&gfx, black_box(context), None)?;
    }
    let unchanged_us = start.elapsed().as_micros();
    assert!(panel.target.Root()? == root);
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = panel.action(0);
    }
    let keyboard_us = start.elapsed().as_micros();
    drop(panel);
    drop(card);
    let mut times = Vec::with_capacity(80);
    let mut churn_private = Vec::with_capacity(2);
    for round in 0..80 {
        let start = Instant::now();
        drop(Popup::new(&gfx, context, &data)?);
        times.push(start.elapsed().as_micros() as u64);
        // Real popup closes return to the dispatcher before the next open.
        // Drain its work rather than accumulating forty uncommitted trees.
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                unsafe {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if round == 39 || round == 79 {
            // Same coalesced, non-blocking maintenance as the app's idle timer.
            gfx.flush_released_drawings()?;
            // A hidden target has no present cycle. Explicitly commit its
            // pending creates/deletes, then pump until the async batch settles.
            let commit = gfx.compositor.RequestCommitAsync()?;
            idle()?;
            anyhow::ensure!(
                commit.Status()? == windows_future::AsyncStatus::Completed,
                "the hidden compositor batch has not completed"
            );
            churn_private.push(private_bytes()?);
        }
    }
    times.sort_unstable();
    idle()?;
    let final_private = private_bytes()?;
    anyhow::ensure!(
        churn_private[1].saturating_sub(churn_private[0]) < 16 * 1024 * 1024,
        "weather retained more than 16 MiB in the second batch: {:?}",
        churn_private
    );
    let final_user = unsafe { GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS) };
    assert!(
        final_user <= baseline_user + 1,
        "opening repeatedly must not accumulate USER handles"
    );
    println!(
        "{}",
        serde_json::json!({ "scope": "hidden owned native forecast popup; CPU of owning UI thread; excludes GPU/DWM/network/location",
        "profile": "release", "harness_lto_disabled": option_env!("CARGO_PROFILE_RELEASE_LTO") == Some("false"), "open_close_samples": times.len(), "open_close_median_us": times[times.len()/2],
        "open_close_p95_us": times[times.len()*95/100], "unchanged_10000_us": unchanged_us,
        "keyboard_1000_us": keyboard_us, "one_second_idle_off_thread_us": idle_off_us,
        "one_second_idle_panel_thread_us": idle_panel_us, "one_second_idle_card_thread_us": idle_card_us,
        "private_bytes_with_card": card_private, "private_bytes_baseline": baseline_private,
        "private_bytes_with_panel": open_private, "private_bytes_after_churn_closed": final_private, "private_bytes_after_40_and_80": churn_private, "user_handles_before": baseline_user, "user_handles_after": final_user })
    );
    Ok(())
}
