//! HNSW (Hierarchical Navigable Small World) インデックス
//!
//! 多層グラフ構造による近似最近傍探索。
//! 線形スキャン O(N) → O(log N) に改善。
//!
//! ファイルフォーマット (.hnsw):
//!   [MAGIC: 4B "HNW1"]
//!   [M: u32][ef_construction: u32]
//!   [entry_point: i64]  (-1 = 空)
//!   [max_layer: u32]
//!   [node_count: u32]
//!   各ノード:
//!     [id_len: u16][id bytes]
//!     [vector_dim: u32][f32 x dim]
//!     [level: u32]
//!     for lc in 0..=level:
//!       [conn_count: u32][node_idx: u32 x conn_count]
use anyhow::Result;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rand::Rng;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"HNW1";
const DEFAULT_M: usize = 16;
const DEFAULT_EF_CONSTRUCTION: usize = 200;
const MAX_LEVEL: usize = 16;

fn cosine_dist(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 1.0;
    }
    1.0 - dot / (na * nb)
}

#[derive(Clone)]
struct Node {
    id: String,
    vector: Vec<f32>,
    level: usize,
    connections: Vec<Vec<usize>>, // connections[layer] = [node_idx, ...]
}

// max-heap by distance（W セット: ef 件の中で最遠をすぐ取り出す）
#[derive(PartialEq)]
struct Far(f32, usize);
impl Eq for Far {}
impl PartialOrd for Far {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Far {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(Ordering::Equal)
    }
}

// min-heap by distance（C セット: 最近傍から優先的に探索）
#[derive(PartialEq)]
struct Near(f32, usize);
impl Eq for Near {}
impl PartialOrd for Near {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Near {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.partial_cmp(&self.0).unwrap_or(Ordering::Equal) // 逆順
    }
}

pub struct Hnsw {
    m: usize,
    m_max0: usize,         // layer 0 は接続数を 2*M まで許容
    ef_construction: usize,
    ml: f64,               // レベル生成の倍率 = 1/ln(M)
    entry_point: Option<usize>,
    max_layer: usize,
    nodes: Vec<Node>,
}

impl Hnsw {
    pub fn new() -> Self {
        let m = DEFAULT_M;
        Self {
            m,
            m_max0: m * 2,
            ef_construction: DEFAULT_EF_CONSTRUCTION,
            ml: 1.0 / (m as f64).ln(),
            entry_point: None,
            max_layer: 0,
            nodes: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 指数分布によるレベル生成（高層ほど確率が低い）
    fn random_level(&self) -> usize {
        let r = rand::thread_rng().gen::<f64>().max(f64::MIN_POSITIVE);
        ((-r.ln() * self.ml).floor() as usize).min(MAX_LEVEL)
    }

    fn connections_at(&self, node: usize, layer: usize) -> &[usize] {
        let n = &self.nodes[node];
        if layer < n.connections.len() {
            &n.connections[layer]
        } else {
            &[]
        }
    }

    /// 1 件だけ返す貪欲探索（上層の高速ナビゲーション用）
    fn greedy_search(&self, q: &[f32], ep: usize, layer: usize) -> usize {
        let mut cur = ep;
        let mut cur_d = cosine_dist(q, &self.nodes[ep].vector);
        loop {
            let mut improved = false;
            for &nb in self.connections_at(cur, layer) {
                let d = cosine_dist(q, &self.nodes[nb].vector);
                if d < cur_d {
                    cur_d = d;
                    cur = nb;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        cur
    }

    /// ef 件を返すビームサーチ（実際の近傍収集用）
    fn beam_search(&self, q: &[f32], ep: usize, ef: usize, layer: usize) -> Vec<usize> {
        let ep_d = cosine_dist(q, &self.nodes[ep].vector);
        let mut visited = HashSet::from([ep]);
        let mut cands = BinaryHeap::from([Near(ep_d, ep)]);
        let mut found = BinaryHeap::from([Far(ep_d, ep)]);

        while let Some(Near(c_d, c)) = cands.pop() {
            let f_d = found.peek().map(|Far(d, _)| *d).unwrap_or(f32::MAX);
            if c_d > f_d {
                break; // 未探索の最近傍 > 発見済み最遠傍 → 改善不可
            }
            for &e in self.connections_at(c, layer) {
                if !visited.insert(e) {
                    continue;
                }
                let e_d = cosine_dist(q, &self.nodes[e].vector);
                let f_d = found.peek().map(|Far(d, _)| *d).unwrap_or(f32::MAX);
                if e_d < f_d || found.len() < ef {
                    cands.push(Near(e_d, e));
                    found.push(Far(e_d, e));
                    if found.len() > ef {
                        found.pop(); // 最遠を削除
                    }
                }
            }
        }
        found.into_iter().map(|Far(_, idx)| idx).collect()
    }

    /// 候補の中から q に最も近い m 件を選択
    fn select_neighbors(&self, q: &[f32], candidates: &[usize], m: usize) -> Vec<usize> {
        let mut scored: Vec<(f32, usize)> = candidates
            .iter()
            .map(|&i| (cosine_dist(q, &self.nodes[i].vector), i))
            .collect();
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        scored.iter().take(m).map(|&(_, i)| i).collect()
    }

    /// ベクトルをインデックスに挿入
    pub fn insert(&mut self, id: String, vector: Vec<f32>) {
        let q = self.nodes.len();
        let level = self.random_level();
        self.nodes.push(Node {
            id,
            vector,
            level,
            connections: (0..=level).map(|_| Vec::new()).collect(),
        });

        // 最初のノードはそのままエントリポイントに
        let Some(mut ep) = self.entry_point else {
            self.entry_point = Some(0);
            self.max_layer = level;
            return;
        };

        let l = self.max_layer;
        let q_vec = self.nodes[q].vector.clone();

        // Phase 1: 上層から level+1 まで貪欲に降りてエントリポイントを絞り込む
        for lc in (level + 1..=l).rev() {
            ep = self.greedy_search(&q_vec, ep, lc);
        }

        // Phase 2: 0..=min(level, l) の各層でビームサーチ + 双方向接続
        for lc in (0..=level.min(l)).rev() {
            let cands = self.beam_search(&q_vec, ep, self.ef_construction, lc);
            let m_lim = if lc == 0 { self.m_max0 } else { self.m };
            let neighbors = self.select_neighbors(&q_vec, &cands, m_lim);

            self.nodes[q].connections[lc] = neighbors.clone();

            for &e in &neighbors {
                self.nodes[e].connections[lc].push(q);
                // 接続数が上限を超えたら剪定
                if self.nodes[e].connections[lc].len() > m_lim {
                    let e_vec = self.nodes[e].vector.clone();
                    let e_conns = self.nodes[e].connections[lc].clone();
                    self.nodes[e].connections[lc] =
                        self.select_neighbors(&e_vec, &e_conns, m_lim);
                }
            }

            // 次の層（より下）のエントリポイントを候補の最近傍に更新
            ep = cands
                .iter()
                .copied()
                .min_by(|&a, &b| {
                    cosine_dist(&q_vec, &self.nodes[a].vector)
                        .partial_cmp(&cosine_dist(&q_vec, &self.nodes[b].vector))
                        .unwrap_or(Ordering::Equal)
                })
                .unwrap_or(ep);
        }

        // このノードが最上層に達したらエントリポイントを更新
        if level > l {
            self.entry_point = Some(q);
            self.max_layer = level;
        }
    }

    /// 近傍 k 件を探索。戻り値: (cosine_similarity, record_id)
    pub fn search(&self, query: &[f32], k: usize, ef: usize) -> Vec<(f32, &str)> {
        let Some(mut ep) = self.entry_point else {
            return vec![];
        };

        // 上層から layer 1 まで貪欲に降りる
        for lc in (1..=self.max_layer).rev() {
            ep = self.greedy_search(query, ep, lc);
        }

        // layer 0 でビームサーチ
        let cands = self.beam_search(query, ep, ef.max(k), 0);
        let mut results: Vec<(f32, &str)> = cands
            .iter()
            .map(|&i| {
                let sim = 1.0 - cosine_dist(query, &self.nodes[i].vector);
                (sim, self.nodes[i].id.as_str())
            })
            .collect();
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        results.truncate(k);
        results
    }

    /// 既存レコード群からインデックスを一括構築
    pub fn build(items: impl Iterator<Item = (String, Vec<f32>)>) -> Self {
        let mut h = Self::new();
        for (id, vec) in items {
            h.insert(id, vec);
        }
        h
    }

    /// .hnsw ファイルに保存
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut f = std::fs::File::create(path)?;
        f.write_all(MAGIC)?;
        f.write_u32::<LittleEndian>(self.m as u32)?;
        f.write_u32::<LittleEndian>(self.ef_construction as u32)?;
        f.write_i64::<LittleEndian>(self.entry_point.map(|e| e as i64).unwrap_or(-1))?;
        f.write_u32::<LittleEndian>(self.max_layer as u32)?;
        f.write_u32::<LittleEndian>(self.nodes.len() as u32)?;

        for node in &self.nodes {
            let id_b = node.id.as_bytes();
            f.write_u16::<LittleEndian>(id_b.len() as u16)?;
            f.write_all(id_b)?;

            f.write_u32::<LittleEndian>(node.vector.len() as u32)?;
            for &v in &node.vector {
                f.write_f32::<LittleEndian>(v)?;
            }

            f.write_u32::<LittleEndian>(node.level as u32)?;
            for lc in 0..=node.level {
                let c = &node.connections[lc];
                f.write_u32::<LittleEndian>(c.len() as u32)?;
                for &i in c {
                    f.write_u32::<LittleEndian>(i as u32)?;
                }
            }
        }
        Ok(())
    }

    /// .hnsw ファイルから読み込み（存在しなければ空インデックスを返す）
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let mut f = std::fs::File::open(path)?;

        let mut magic = [0u8; 4];
        f.read_exact(&mut magic)?;
        anyhow::ensure!(&magic == MAGIC, "invalid HNSW file format");

        let m = f.read_u32::<LittleEndian>()? as usize;
        let ef_construction = f.read_u32::<LittleEndian>()? as usize;
        let ep_raw = f.read_i64::<LittleEndian>()?;
        let entry_point = if ep_raw < 0 { None } else { Some(ep_raw as usize) };
        let max_layer = f.read_u32::<LittleEndian>()? as usize;
        let n = f.read_u32::<LittleEndian>()? as usize;

        let mut nodes = Vec::with_capacity(n);
        for _ in 0..n {
            let id_len = f.read_u16::<LittleEndian>()? as usize;
            let mut id_b = vec![0u8; id_len];
            f.read_exact(&mut id_b)?;
            let id = String::from_utf8(id_b)?;

            let dim = f.read_u32::<LittleEndian>()? as usize;
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                vector.push(f.read_f32::<LittleEndian>()?);
            }

            let level = f.read_u32::<LittleEndian>()? as usize;
            let mut connections = Vec::with_capacity(level + 1);
            for _ in 0..=level {
                let cn = f.read_u32::<LittleEndian>()? as usize;
                let mut c = Vec::with_capacity(cn);
                for _ in 0..cn {
                    c.push(f.read_u32::<LittleEndian>()? as usize);
                }
                connections.push(c);
            }
            nodes.push(Node { id, vector, level, connections });
        }

        Ok(Self {
            m,
            m_max0: m * 2,
            ef_construction,
            ml: 1.0 / (m as f64).ln(),
            entry_point,
            max_layer,
            nodes,
        })
    }
}
