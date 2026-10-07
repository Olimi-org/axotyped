/// An error which may occur when exporting a type
#[derive(thiserror::Error, Debug)]
pub enum ExportError {
    /// A type reached the exporter with nowhere to write. The payload names
    /// it — a diagnostic that cannot name its subject is not a diagnostic.
    #[error("`{0}` has no export path: it is a primitive inlined into its users, or was not registered for export")]
    CannotBeExported(&'static str),
    #[cfg(feature = "format")]
    #[error("an error occurred while formatting the generated typescript output")]
    Formatting(String),
    #[error("an error occurred while performing IO")]
    Io(#[from] std::io::Error),
    #[error("the environment variable CARGO_MANIFEST_DIR is not set")]
    ManifestDirNotSet,
    #[error("an error occurred while writing to a formatted buffer")]
    Fmt(#[from] std::fmt::Error),
    #[error(r#"TS_RS_IMPORT_EXTENSION must be either "js" or "ts""#)]
    InvalidImportExtension,
}

#[cfg(test)]
mod tests {
    use super::ExportError;

    /// The payload is the point of the variant — a dropped payload turns a
    /// generation warning into a shrug.
    #[test]
    fn cannot_be_exported_names_the_type() {
        let msg = ExportError::CannotBeExported("my_crate::Row").to_string();
        assert!(msg.contains("my_crate::Row"), "{msg}");
    }
}
