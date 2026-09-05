use super::*;

fn create_io_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")
}

fn create_serde_error() -> serde_json::Error {
    serde_json::from_str::<serde_json::Value>("{abcdefg}").unwrap_err()
}

#[test]
fn test_err_display() {
    let src = create_io_error();
    let io = AppError::Io {
        operation: "open the file",
        path: "test1.json".to_string(),
        source: src,
    };
    assert_eq!(
        io.to_string(),
        "failed to open the file 'test1.json': no such file"
    );

    let corrupted = AppError::Corrupted {
        path: "test2.json".to_string(),
        source: create_serde_error(),
    };
    assert_eq!(
        corrupted.to_string(),
        format!(
            "task file 'test2.json' is corrupted: {}",
            corrupted.source().unwrap()
        )
    );

    let do_not_found_any_file = AppError::DoNotFoundAnyFiles;
    assert_eq!(do_not_found_any_file.to_string(), "Do not found any files");
}
