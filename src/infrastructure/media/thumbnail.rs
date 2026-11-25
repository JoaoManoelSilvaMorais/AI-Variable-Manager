use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use crate::domain::asset::ImageFormat;
use crate::domain::ports::{ThumbnailCache, ThumbnailData, ThumbnailState};

pub const THUMBNAIL_MAX_ENTRIES: usize = 192;
const THUMBNAIL_MAX_DIMENSION: u32 = 96;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    asset_id: String,
    path: PathBuf,
}

struct LruStore {
    capacity: usize,
    generation: u64,
    entries: HashMap<CacheKey, Arc<ThumbnailData>>,
    order: VecDeque<CacheKey>,
    in_flight: HashSet<CacheKey>,
    recent_failures: HashMap<CacheKey, String>,
}

impl LruStore {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            generation: 0,
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            in_flight: HashSet::new(),
            recent_failures: HashMap::new(),
        }
    }

    fn touch_lru(&mut self, key: &CacheKey) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.clone());
    }

    fn insert_ready(&mut self, key: CacheKey, data: Arc<ThumbnailData>) {
        if self.entries.contains_key(&key) {
            self.entries.insert(key.clone(), data);
            self.touch_lru(&key);
            return;
        }
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.entries.insert(key.clone(), data);
        self.order.push_back(key);
    }
}

struct DecodeJob {
    key: CacheKey,
    generation: u64,
}

/// Thread-safe, bounded (<= 192 entries) LRU thumbnail cache decoding images off-thread.
pub struct LruThumbnailCache {
    store: Arc<Mutex<LruStore>>,
    tx: mpsc::Sender<DecodeJob>,
}

impl Default for LruThumbnailCache {
    fn default() -> Self {
        Self::new(THUMBNAIL_MAX_ENTRIES)
    }
}

impl LruThumbnailCache {
    pub fn new(capacity: usize) -> Self {
        let store = Arc::new(Mutex::new(LruStore::new(capacity)));
        let (tx, rx) = mpsc::channel::<DecodeJob>();
        let worker_store = Arc::clone(&store);

        thread::Builder::new()
            .name("aivm-thumb-worker".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let result = decode_thumbnail_96x96(&job.key.path);
                    let mut guard = match worker_store.lock() {
                        Ok(g) => g,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    guard.in_flight.remove(&job.key);
                    if guard.generation != job.generation {
                        continue;
                    }
                    match result {
                        Ok(data) => {
                            guard.recent_failures.remove(&job.key);
                            guard.insert_ready(job.key, Arc::new(data));
                        }
                        Err(msg) => {
                            guard.recent_failures.insert(job.key, msg);
                        }
                    }
                }
            })
            .expect("spawn thumbnail worker thread");

        Self { store, tx }
    }
}

fn decode_thumbnail_96x96(path: &Path) -> Result<ThumbnailData, String> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let declared_format = ImageFormat::from_extension(ext)
        .ok_or_else(|| format!("Extensão de imagem não suportada: '.{ext}'"))?;

    let meta = std::fs::metadata(path)
        .map_err(|e| format!("Não foi possível acessar '{}': {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("Caminho não é um arquivo: {}", path.display()));
    }
    if meta.len() == 0 {
        return Err(format!("Arquivo de imagem vazio (0 bytes): {}", path.display()));
    }

    let reader = image::ImageReader::open(path)
        .map_err(|e| format!("Falha ao abrir imagem: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("Falha ao detectar formato da imagem: {e}"))?;

    let decoded = match reader.decode() {
        Ok(img) => img,
        Err(first_err) => {
            let fallback_fmt = match declared_format {
                ImageFormat::Jpg | ImageFormat::Jpeg => image::ImageFormat::Jpeg,
                ImageFormat::Png => image::ImageFormat::Png,
                ImageFormat::Tiff | ImageFormat::Tif => image::ImageFormat::Tiff,
            };
            let bytes = std::fs::read(path)
                .map_err(|e| format!("Falha ao ler bytes da imagem: {e}"))?;
            image::load_from_memory_with_format(&bytes, fallback_fmt)
                .map_err(|_| format!("Imagem corrompida ou não decodificável: {first_err}"))?
        }
    };

    let thumb = decoded.thumbnail(THUMBNAIL_MAX_DIMENSION, THUMBNAIL_MAX_DIMENSION);
    let rgba = thumb.to_rgba8();
    let (width, height) = rgba.dimensions();

    Ok(ThumbnailData {
        width,
        height,
        rgba8: rgba.into_raw(),
    })
}

impl ThumbnailCache for LruThumbnailCache {
    fn get_or_request(&self, asset_id: &str, path: &Path) -> ThumbnailState {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if ImageFormat::from_extension(ext).is_none() {
            return ThumbnailState::Failed(format!("Formato não suportado: '.{ext}'"));
        }
        if !path.is_file() {
            return ThumbnailState::Failed(format!("Arquivo ausente no disco: {}", path.display()));
        }

        let key = CacheKey {
            asset_id: asset_id.to_string(),
            path: path.to_path_buf(),
        };

        let mut guard = match self.store.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        if let Some(cached) = guard.entries.get(&key).cloned() {
            guard.touch_lru(&key);
            return ThumbnailState::Ready(cached);
        }

        if let Some(err_msg) = guard.recent_failures.remove(&key) {
            return ThumbnailState::Failed(err_msg);
        }

        if guard.in_flight.contains(&key) {
            return ThumbnailState::Loading;
        }

        let generation = guard.generation;
        guard.in_flight.insert(key.clone());
        drop(guard);

        if self.tx.send(DecodeJob { key: key.clone(), generation }).is_err() {
            let mut g = self.store.lock().unwrap_or_else(|p| p.into_inner());
            g.in_flight.remove(&key);
            return ThumbnailState::Failed("Worker de miniaturas indisponível.".to_string());
        }

        ThumbnailState::Loading
    }

    fn clear(&self) {
        let mut guard = match self.store.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.generation = guard.generation.wrapping_add(1);
        guard.entries.clear();
        guard.order.clear();
        guard.recent_failures.clear();
    }

    fn len(&self) -> usize {
        let guard = match self.store.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.entries.len()
    }
}
