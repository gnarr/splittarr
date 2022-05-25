mod settings;

use settings::Settings;

fn main() {
    println!("Splittarr");

    let settings = Settings::new();
    println!("{:?}", settings);
}
