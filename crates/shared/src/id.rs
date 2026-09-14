use ulid::Ulid;

pub fn generate(prefix: &str) -> String {
    let ulid = Ulid::new();
    format!("{}_{}", prefix, ulid.to_string().to_lowercase())
}
