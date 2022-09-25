use crate::settings::Settings;
use crate::Download;
use itertools::Itertools;
use rcue::parser::parse_from_file;
use regex::Regex;
use std::process::Command;
use walkdir::DirEntry;

pub async fn split(download: &mut Download, cue_entry: DirEntry) {
    let settings = Settings::new().unwrap();
    let cue_path = cue_entry.path().to_str().unwrap();
    let cue_file = download.add_cue_file(cue_path.to_owned()).await.unwrap();
    let cue = parse_from_file(cue_path, settings.cue.strict).unwrap();
    println!(
        "Processing {} by {}",
        cue.title.unwrap(),
        cue.performer.unwrap()
    );

    let cue_dir = cue_entry.path().parent().unwrap().to_str().unwrap();

    let cue_file_name = cue_entry.file_name().to_str().unwrap();

    let overwrite = if settings.shnsplit.overwrite {
        "always"
    } else {
        "never"
    };

    let mut args = vec![
        "-f",
        cue_file_name,
        "-d",
        cue_dir,
        "-t",
        &settings.shnsplit.format,
        "-O",
        overwrite,
        "-o",
        "flac flac -s -8 -o %f -",
    ];

    let mut files = Vec::new();
    for file in cue.files {
        files.push(file.file.to_owned());
    }
    let files: Vec<String> = files.into_iter().unique().collect();
    for file in &files {
        args.push(file);
    }

    let shnsplit_output = Command::new(settings.shnsplit.path)
        .current_dir(cue_dir)
        .args(args)
        .output()
        .unwrap();

    let stderr_buffer = shnsplit_output.stderr;
    let stderr = std::str::from_utf8(&stderr_buffer).unwrap();

    let re = Regex::new(r"Splitting \[(?P<input_file>[^]]+)] \((?P<input_length>[^)]+)\) --> \[(?P<output_file>[^]]+)] \((?P<output_length>[^)]+)\) :").unwrap();

    for cap in re.captures_iter(stderr) {
        let output_file = &cap["output_file"];
        cue_file.add_track(output_file.to_string()).await;
        println!("{}", output_file);
    }
}
