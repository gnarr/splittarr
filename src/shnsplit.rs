pub mod shnsplit {
    use itertools::Itertools;
    use rcue::parser::parse_from_file;
    use regex::Regex;
    use settings::Settings;
    use std::borrow::Borrow;
    use std::process::Command;
    use walkdir::DirEntry;

    use crate::settings::settings;
    use crate::settings::settings::Shnsplit;

    pub fn split(cue_entry: DirEntry) {
        let cue_path = cue_entry.path().to_str().unwrap();
        dbg!(cue_path);
        let cue = parse_from_file(cue_path, true).unwrap();
        println!(
            "Processing {} by {}",
            cue.title.unwrap(),
            cue.performer.unwrap()
        );

        let cue_dir = cue_entry.path().parent().unwrap().to_str().unwrap();
        dbg!(cue_dir);

        let cue_file_name = cue_entry.file_name().to_str().unwrap();

        let settings = Settings::new();
        let shnsplit = settings.get::<Shnsplit>("shnsplit").unwrap();
        let overwrite = if shnsplit.overwrite.to_owned() {
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
            "%n - %t",
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
        for i in 0..files.len() {
            args.push(files[i].borrow());
        }

        let shnsplit_path = shnsplit.path.to_owned();
        let shnsplit_output = Command::new(shnsplit_path)
            .current_dir(cue_dir)
            .args(args)
            .output()
            .unwrap();

        let stderr_buffer = shnsplit_output.stderr;
        let stderr = std::str::from_utf8(&stderr_buffer).unwrap();

        let re = Regex::new(r"Splitting \[(?P<input_file>[^]]+)] \((?P<input_length>[^)]+)\) --> \[(?P<output_file>[^]]+)] \((?P<output_length>[^)]+)\) :").unwrap();

        for cap in re.captures_iter(stderr) {
            let input_file = &cap["input_file"];
            let input_length = &cap["input_length"];
            let output_file = &cap["output_file"];
            let output_length = &cap["output_length"];
            dbg!(input_file);
            dbg!(input_length);
            dbg!(output_file);
            dbg!(output_length);
        }
    }
}
