use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFinding {
    pub id: usize,
    pub source: String,
    pub claim: String,
    pub details: String,
    pub caveats_or_contradiction: Option<String>,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContradictionNote {
    pub id: usize,
    pub point_of_conflict: String,
    pub claim_a: String,
    pub source_a: String,
    pub claim_b: String,
    pub source_b: String,
    pub technical_analysis: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResearchBoard {
    pub topic: String,
    pub findings: Vec<ResearchFinding>,
    pub contradictions: Vec<ContradictionNote>,
    pub verified_facts: Vec<String>,
}

impl ResearchBoard {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_string(),
            findings: Vec::new(),
            contradictions: Vec::new(),
            verified_facts: Vec::new(),
        }
    }

    pub fn record_finding(
        &mut self,
        source: &str,
        claim: &str,
        details: &str,
        caveats: Option<&str>,
        verified: bool,
    ) -> usize {
        let id = self.findings.len() + 1;
        self.findings.push(ResearchFinding {
            id,
            source: source.trim().to_string(),
            claim: claim.trim().to_string(),
            details: details.trim().to_string(),
            caveats_or_contradiction: caveats.map(|c| c.trim().to_string()),
            verified,
        });

        if verified && !self.verified_facts.iter().any(|f| f == claim.trim()) {
            self.verified_facts.push(claim.trim().to_string());
        }

        id
    }

    pub fn record_contradiction(
        &mut self,
        point_of_conflict: &str,
        claim_a: &str,
        source_a: &str,
        claim_b: &str,
        source_b: &str,
        technical_analysis: &str,
    ) -> usize {
        let id = self.contradictions.len() + 1;
        self.contradictions.push(ContradictionNote {
            id,
            point_of_conflict: point_of_conflict.trim().to_string(),
            claim_a: claim_a.trim().to_string(),
            source_a: source_a.trim().to_string(),
            claim_b: claim_b.trim().to_string(),
            source_b: source_b.trim().to_string(),
            technical_analysis: technical_analysis.trim().to_string(),
        });
        id
    }

    pub fn summary_brief(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "📋 [RESEARCH BOARD: {} | {} Findings, {} Verified Facts, {} Contradictions Tracked]\n",
            if self.topic.len() > 40 {
                format!("{}...", &self.topic[..37])
            } else {
                self.topic.clone()
            },
            self.findings.len(),
            self.verified_facts.len(),
            self.contradictions.len()
        ));

        if !self.verified_facts.is_empty() {
            out.push_str("✓ Verified Core Facts:\n");
            for (idx, fact) in self.verified_facts.iter().take(5).enumerate() {
                out.push_str(&format!("  {}. {}\n", idx + 1, fact));
            }
            if self.verified_facts.len() > 5 {
                out.push_str(&format!(
                    "  [... {} more verified facts in memory ...]\n",
                    self.verified_facts.len() - 5
                ));
            }
        }

        if !self.contradictions.is_empty() {
            out.push_str("⚖️ Tracked Contradictions / Nuances:\n");
            for c in &self.contradictions {
                out.push_str(&format!(
                    "  • #{}: {} (A: '{}' vs B: '{}')\n",
                    c.id, c.point_of_conflict, c.claim_a, c.claim_b
                ));
            }
        }

        out
    }

    pub fn full_evidence_dossier(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# Research Evidence Board: {}\n\n", self.topic));

        out.push_str("## Accumulated Findings\n");
        if self.findings.is_empty() {
            out.push_str("(No findings recorded yet)\n\n");
        } else {
            for f in &self.findings {
                out.push_str(&format!(
                    "### [{}] Finding #{}: {}\n- **Source:** {}\n- **Verified:** {}\n- **Technical Details:** {}\n",
                    if f.verified { "VERIFIED" } else { "HYPOTHESIS" },
                    f.id,
                    f.claim,
                    f.source,
                    f.verified,
                    f.details
                ));
                if let Some(ref cav) = f.caveats_or_contradiction {
                    out.push_str(&format!("- **Caveats/Contradictions:** {}\n", cav));
                }
                out.push('\n');
            }
        }

        if !self.contradictions.is_empty() {
            out.push_str("## Technical Contradictions & Differing Perspectives\n");
            for c in &self.contradictions {
                out.push_str(&format!(
                    "### Conflict #{}: {}\n- **Claim A:** {} (Source: {})\n- **Claim B:** {} (Source: {})\n- **Analysis/Resolution:** {}\n\n",
                    c.id, c.point_of_conflict, c.claim_a, c.source_a, c.claim_b, c.source_b, c.technical_analysis
                ));
            }
        }

        out
    }

    /// Encrypts in-memory research board into an authenticated ciphertext blob
    /// using Silo's VaultKey (ChaCha20-Poly1305 + shake-entropy).
    /// Raw unencrypted data never touches disk.
    #[allow(dead_code)]
    pub fn encrypt_to_bytes(&self, key: &crate::vault::VaultKey) -> Result<Vec<u8>, String> {
        let serialized = postcard::to_allocvec(self)
            .map_err(|e| format!("Postcard serialization error: {}", e))?;
        key.encrypt(&serialized)
    }

    /// Decrypts research board from an authenticated ciphertext blob.
    #[allow(dead_code)]
    pub fn decrypt_from_bytes(blob: &[u8], key: &crate::vault::VaultKey) -> Result<Self, String> {
        let decrypted = key.decrypt(blob)?;
        postcard::from_bytes(&decrypted)
            .map_err(|e| format!("Postcard deserialization error: {}", e))
    }
}

static ACTIVE_RESEARCH_BOARD: RwLock<Option<ResearchBoard>> = RwLock::new(None);

pub fn init_global_research_board(topic: &str) {
    if let Ok(mut lock) = ACTIVE_RESEARCH_BOARD.write() {
        *lock = Some(ResearchBoard::new(topic));
    }
}

pub fn get_global_board_summary() -> String {
    if let Ok(lock) = ACTIVE_RESEARCH_BOARD.read() {
        if let Some(ref board) = *lock {
            return board.summary_brief();
        }
    }
    String::new()
}

pub fn get_global_evidence_dossier() -> String {
    if let Ok(lock) = ACTIVE_RESEARCH_BOARD.read() {
        if let Some(ref board) = *lock {
            return board.full_evidence_dossier();
        }
    }
    String::new()
}

pub fn record_finding_to_global_board(
    source: &str,
    claim: &str,
    details: &str,
    caveats: Option<&str>,
    verified: bool,
) -> String {
    if let Ok(mut lock) = ACTIVE_RESEARCH_BOARD.write() {
        if let Some(ref mut board) = *lock {
            let id = board.record_finding(source, claim, details, caveats, verified);
            return format!(
                "✓ Finding #{} recorded in Research Board.\n{}",
                id,
                board.summary_brief()
            );
        } else {
            let mut board = ResearchBoard::new("Unassigned Deep Research Session");
            let id = board.record_finding(source, claim, details, caveats, verified);
            let summary = board.summary_brief();
            *lock = Some(board);
            return format!(
                "✓ Finding #{} recorded in new Research Board.\n{}",
                id, summary
            );
        }
    }
    "Error: Unable to lock Research Board for write.".to_string()
}

pub fn record_contradiction_to_global_board(
    point: &str,
    claim_a: &str,
    source_a: &str,
    claim_b: &str,
    source_b: &str,
    analysis: &str,
) -> String {
    if let Ok(mut lock) = ACTIVE_RESEARCH_BOARD.write() {
        if let Some(ref mut board) = *lock {
            let id = board.record_contradiction(point, claim_a, source_a, claim_b, source_b, analysis);
            return format!(
                "✓ Contradiction #{} recorded in Research Board.\n{}",
                id,
                board.summary_brief()
            );
        }
    }
    "Error: Unable to lock Research Board for write.".to_string()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultKey;

    #[test]
    fn test_research_board_operations_and_vault_encryption() {
        let mut board = ResearchBoard::new("ML-KEM Side-Channels");
        let id1 = board.record_finding(
            "https://kyberslash.cr.yp.to/",
            "KyberSlash timing attack",
            "Variable-time division in division/poly_tomsg",
            Some("Fixed by replacing division with constant-time multiply and shift"),
            true,
        );
        assert_eq!(id1, 1);

        let id2 = board.record_contradiction(
            "Constant-time guarantees under compiler optimization",
            "Pure Rust subtle crate prevents timing side-channels",
            "Rust crypto guidelines",
            "LLVM peephole optimizations can introduce branches on conditional select if not using asm barriers",
            "Almeida et al. (TCHES 2020)",
            "Compiler backends can transform branchless code into conditional jumps",
        );
        assert_eq!(id2, 1);

        let summary = board.summary_brief();
        assert!(summary.contains("KyberSlash timing attack"));
        assert!(summary.contains("Constant-time guarantees"));

        // Encrypt in-memory board using VaultKey (ChaCha20-Poly1305 + shake-entropy)
        let key = VaultKey::generate_random();
        let encrypted_blob = board.encrypt_to_bytes(&key).expect("Encryption failed");
        assert!(encrypted_blob.starts_with(b"SILO\x01"));

        // Decrypt and verify full fidelity
        let restored = ResearchBoard::decrypt_from_bytes(&encrypted_blob, &key)
            .expect("Decryption failed");
        assert_eq!(restored.topic, "ML-KEM Side-Channels");
        assert_eq!(restored.findings.len(), 1);
        assert_eq!(restored.contradictions.len(), 1);
        assert_eq!(restored.findings[0].claim, "KyberSlash timing attack");
    }
}
