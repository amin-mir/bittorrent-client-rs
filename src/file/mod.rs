// TODO: Implement FileWriter and Write pieces to a preallocated file.

use std::path::{Path, PathBuf};

use tokio::fs;

use crate::bencode::DownloadType;

/// In case of single file download, output is the name of the file.
/// In case of multi file download, output is the directory name
/// where all torrent files will be downloaded to.
pub async fn write(output: String, dl_type: DownloadType, bytes: Vec<u8>) -> anyhow::Result<()> {
    use DownloadType::*;

    match dl_type {
        SingleFile { length: _ } => Ok(tokio::fs::write(output, bytes).await?),
        MultiFile { files } => {
            let output_dir = Path::new(&output);
            let mut idx = 0;
            for f in files {
                let path = output_dir.join(make_file_path(f.path));
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await?;
                }
                let new_idx = idx + f.length as usize;
                tokio::fs::write(path, &bytes[idx..new_idx]).await?;
                idx = new_idx;
            }
            Ok(())
        }
    }
}

fn make_file_path(parts: Vec<String>) -> PathBuf {
    let mut path = PathBuf::new();
    for p in parts {
        path.push(p);
    }
    path
}

#[cfg(test)]
mod tests {
    use crate::bencode::File;
    use tempfile::tempdir;
    use tokio::{fs, io::AsyncReadExt};

    use super::*;

    #[tokio::test]
    async fn test_write() {
        let dir = tempdir().unwrap();

        let download_type = DownloadType::MultiFile {
            files: vec![
                File {
                    length: 4,
                    path: vec![
                        "path".to_string(),
                        "to".to_string(),
                        "file1.txt".to_string(),
                    ],
                },
                File {
                    length: 6,
                    path: vec!["path".to_string(), "file2.txt".to_string()],
                },
                File {
                    length: 3,
                    path: vec!["file3.txt".to_string()],
                },
            ],
        };

        let file_bytes = vec![1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 3, 3, 3];
        println!("{}", dir.path().to_str().unwrap());

        write(
            dir.path().to_str().unwrap().to_owned(),
            download_type,
            file_bytes,
        )
        .await
        .unwrap();

        let mut read_bytes = Vec::with_capacity(10);

        let mut file1 = fs::File::open(dir.path().join("path/to/file1.txt"))
            .await
            .unwrap();
        file1.read_to_end(&mut read_bytes).await.unwrap();
        assert_eq!(vec![1, 1, 1, 1], read_bytes);
        read_bytes.clear();

        let mut file2 = fs::File::open(dir.path().join("path/file2.txt"))
            .await
            .unwrap();
        file2.read_to_end(&mut read_bytes).await.unwrap();
        assert_eq!(vec![2, 2, 2, 2, 2, 2], read_bytes);
        read_bytes.clear();

        let mut file3 = fs::File::open(dir.path().join("file3.txt")).await.unwrap();
        file3.read_to_end(&mut read_bytes).await.unwrap();
        assert_eq!(vec![3, 3, 3], read_bytes);
    }
}
