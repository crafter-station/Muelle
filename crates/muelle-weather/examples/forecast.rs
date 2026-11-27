//! One explicit public weather request for supplied coordinates. No Windows
//! location access, background loop, files, desktop changes or precise logs.
use muelle_weather::{ATTRIBUTION, Client, Coordinates};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [latitude, longitude] = arguments.as_slice() else {
        return Err(
            "usage: cargo run -p muelle-weather --example forecast -- <latitude> <longitude>"
                .into(),
        );
    };
    let coordinates = Coordinates::new(latitude.parse()?, longitude.parse()?)?;
    let started = std::time::Instant::now();
    let forecast = Client::new()?.fetch(coordinates)?;
    println!(
        "{:.1} °C · {:?} · zone {} · {} hours · {} days · {:.0} ms\n{}",
        forecast.current.temperature_c,
        forecast.current.condition,
        forecast.timezone,
        forecast.hours.len(),
        forecast.days.len(),
        started.elapsed().as_secs_f64() * 1000.0,
        ATTRIBUTION
    );
    Ok(())
}
