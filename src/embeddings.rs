use anyhow::{bail, Result};
use ndarray::Array2;
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokenizers::Tokenizer;

use crate::util;

const MODEL_DIR_DEFAULT: &str = "~/.memfs/models";
const MODEL_FILENAME: &str = "model.onnx";
const TOKENIZER_FILENAME: &str = "tokenizer.json";
const MODEL_URL: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
pub const EMBEDDING_DIM: usize = 384;
pub const MODEL_VERSION: &str = "all-MiniLM-L6-v2";

pub struct Embedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// Try to load the model. Returns Ok(None) if model files are not present.
    pub fn try_load() -> Result<Option<Self>> {
        let model_dir = model_dir();
        let model_path = model_dir.join(MODEL_FILENAME);
        let tokenizer_path = model_dir.join(TOKENIZER_FILENAME);

        if !model_path.exists() || !tokenizer_path.exists() {
            return Ok(None);
        }

        Ok(Some(Self::load_from(&model_path, &tokenizer_path)?))
    }

    /// Load the model, downloading if necessary.
    pub fn load_or_download() -> Result<Self> {
        let model_dir = model_dir();
        let model_path = model_dir.join(MODEL_FILENAME);
        let tokenizer_path = model_dir.join(TOKENIZER_FILENAME);

        if !model_path.exists() || !tokenizer_path.exists() {
            std::fs::create_dir_all(&model_dir)?;
            if !model_path.exists() {
                download_file(MODEL_URL, &model_path, "model")?;
            }
            if !tokenizer_path.exists() {
                download_file(TOKENIZER_URL, &tokenizer_path, "tokenizer")?;
            }
        }

        Self::load_from(&model_path, &tokenizer_path)
    }

    fn load_from(model_path: &Path, tokenizer_path: &Path) -> Result<Self> {
        let session = Session::builder()?.commit_from_file(model_path)?;
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {}", e))?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
        })
    }

    /// Generate an embedding vector for the given text.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenization failed: {}", e))?;

        let ids = encoding.get_ids();
        let mask = encoding.get_attention_mask();
        let type_ids = encoding.get_type_ids();
        let len = ids.len();

        let input_ids = Tensor::from_array(
            Array2::from_shape_vec((1, len), ids.iter().map(|&x| x as i64).collect())?
        )?;
        let attention_mask = Tensor::from_array(
            Array2::from_shape_vec((1, len), mask.iter().map(|&x| x as i64).collect())?
        )?;
        let token_type_ids = Tensor::from_array(
            Array2::from_shape_vec((1, len), type_ids.iter().map(|&x| x as i64).collect())?
        )?;

        let mut session = self.session.lock().unwrap();
        let outputs = session.run(ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
            "token_type_ids" => token_type_ids,
        ])?;

        // Output: try_extract_tensor returns (&Shape, &[f32])
        // Shape is [1, seq_len, 384]
        let (_shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let hidden_dim = EMBEDDING_DIM;

        // Mean pooling over sequence dimension, masked by attention_mask
        let mut pooled = vec![0f32; hidden_dim];
        let mut mask_sum = 0f32;
        for i in 0..len {
            if mask[i] > 0 {
                mask_sum += 1.0;
                let offset = i * hidden_dim;
                for j in 0..hidden_dim {
                    pooled[j] += data[offset + j];
                }
            }
        }
        if mask_sum > 0.0 {
            for val in pooled.iter_mut() {
                *val /= mask_sum;
            }
        }

        // L2 normalize
        let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in pooled.iter_mut() {
                *val /= norm;
            }
        }

        Ok(pooled)
    }

    /// Serialize embedding to bytes for BLOB storage.
    pub fn serialize_embedding(embedding: &[f32]) -> Vec<u8> {
        embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    /// Deserialize embedding from BLOB storage.
    pub fn deserialize_embedding(bytes: &[u8]) -> Result<Vec<f32>> {
        if bytes.len() % 4 != 0 {
            bail!("invalid embedding bytes length: {}", bytes.len());
        }
        Ok(bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect())
    }

    /// Cosine similarity between two normalized vectors.
    pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    pub fn model_version(&self) -> &str {
        MODEL_VERSION
    }
}

fn model_dir() -> PathBuf {
    let dir = std::env::var("MEMFS_MODEL_PATH")
        .unwrap_or_else(|_| MODEL_DIR_DEFAULT.to_string());
    PathBuf::from(util::expand_tilde(&dir))
}

fn download_file(url: &str, dest: &Path, label: &str) -> Result<()> {
    eprintln!("Downloading {} from {}...", label, url);
    let status = std::process::Command::new("curl")
        .args(["-fSL", "-o"])
        .arg(dest)
        .arg(url)
        .status()?;
    if !status.success() {
        bail!("failed to download {}", url);
    }
    Ok(())
}
