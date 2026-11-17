use goku_core::{connect, Recorder, RecorderError};

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("goku-cli: {err}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<(), RecorderError> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("doctor") => doctor(),
        Some("capture") => {
            let dir = args.next().unwrap_or_else(|| "/tmp/goku-test".into());
            let config = goku_core::Config::new(dir)?;
            let recorder = Recorder::new(config)?;
            recorder.start()?;
            Ok(())
        }
        _ => {
            eprintln!("usage: goku-cli doctor | capture <data-dir>");
            Ok(())
        }
    }
}

fn doctor() -> Result<(), RecorderError> {
    let platform = connect()?;
    let report = platform.permissions();
    println!("os {}", platform.os_name());
    println!("screen {:?}", report.screen);
    println!("accessibility {:?}", report.accessibility);
    println!("input {:?}", report.input);
    println!("can_record {}", report.can_record);
    println!("restart_required {}", report.restart_required);
    Ok(())
}
