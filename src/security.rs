use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct Constitutions {
    pub soul: String,
    pub agent: String,
    pub owner: String,
}

pub fn attest_binary(config: &Config) -> Result<()> {
    fs::create_dir_all(&config.ram_moat).context("Failed to create RAM moat directory")?;

    let exe_path = std::fs::read_link("/proc/self/exe").context("Cannot read /proc/self/exe")?;
    let mut file = fs::File::open(&exe_path).context("Cannot open self binary")?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let hash = hex::encode(Sha256::digest(&buf));
    let hash_file = config.ram_moat.join("golden_binary.hash");

    if hash_file.exists() {
        let stored = fs::read_to_string(&hash_file)
            .context("Cannot read golden binary hash")?
            .trim()
            .to_string();
        if stored != hash {
            eprintln!("FATAL: Binary tampered! Expected {stored}, got {hash}");
            std::process::exit(1);
        }
        tracing::info!("Binary attestation: OK");
    } else {
        fs::write(&hash_file, &hash).context("Cannot write golden binary hash")?;
        tracing::info!("Binary hash established: {hash}");
    }

    Ok(())
}

pub fn verify_constitutions(config: &Config) -> Result<Constitutions> {
    let soul = fs::read_to_string("constitutions/SOUL.md")
        .context("Cannot read constitutions/SOUL.md — run from ornith-stack/ directory")?;
    let agent = fs::read_to_string("constitutions/AGENT.md")
        .context("Cannot read constitutions/AGENT.md")?;
    let owner = fs::read_to_string("constitutions/OWNER.md")
        .context("Cannot read constitutions/OWNER.md")?;

    let soul_hash = hex::encode(Sha256::digest(soul.as_bytes()));
    let hash_file = config.ram_moat.join("soul_golden_hash.txt");

    fs::create_dir_all(&config.ram_moat)?;

    if hash_file.exists() {
        let stored = fs::read_to_string(&hash_file)
            .context("Cannot read soul golden hash")?
            .trim()
            .to_string();
        if stored != soul_hash {
            anyhow::bail!("Constitution tampered: SOUL.md hash mismatch");
        }
        tracing::info!("Constitution integrity: OK");
    } else {
        fs::write(&hash_file, &soul_hash).context("Cannot write soul golden hash")?;
        tracing::info!("Soul hash established: {soul_hash}");
    }

    Ok(Constitutions { soul, agent, owner })
}
