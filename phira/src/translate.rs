//! 【话梅】搜索查询扩展：把中文查询翻译成日文/英文，覆盖官方索引的盲区。
//!
//! ## 为什么需要
//!
//! Phira 官方搜索是**后端字符串匹配**。实测「无呼吸义务」能命中
//! `ノンブレス・オブリージュ`（说明后端对热门曲有某种跨语言索引），
//! 但这个映射并不覆盖全部谱面 —— 冷门曲、新曲、翻唱版本往往只能
//! 用原语言标题搜到。
//!
//! 所以客户端把查询**扩展**成多个语言版本分别搜，再合并去重。
//!
//! ## 为什么不直接调翻译 API
//!
//! 实测 `translate.googleapis.com` 在移动网络下直连失败（HTTP 000）。
//! 而且把第三方 API 密钥塞进客户端既不安全也不可控。
//! 所以走我们自己的服务端代理（`{HUAMEI_SITE}/api/hm/translate`），
//! 服务端可以做缓存、换 provider、限流，客户端只发一个短请求。
//!
//! ## 失败策略
//!
//! 翻译是**增强**，不是必需。任何失败都退回「只用原文搜」——
//! 用户永远不会因为翻译服务挂了就搜不到东西。

use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;

/// 查询翻译缓存。同一首歌会被反复搜，缓存能省掉绝大多数请求。
///
/// key 是原文，value 是展开后的查询列表（含原文本身）。
static CACHE: Lazy<Mutex<HashMap<String, Vec<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// 缓存上限。超出就整体清空 —— 简单可靠，不值得为它引入 LRU。
const CACHE_LIMIT: usize = 256;

/// 把查询扩展成多语言版本。
///
/// 返回值**总是包含原文**，且至少有一个元素 —— 调用方可以无条件遍历。
pub async fn expand(query: &str) -> Vec<String> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }

    if let Some(hit) = CACHE.lock().unwrap().get(q) {
        return hit.clone();
    }

    /* 只对「看起来是中文」的查询做扩展。
       纯 ASCII（英文/罗马字）和纯假名没必要翻译 —— 那只会多两次请求。 */
    if !contains_cjk(q) {
        return vec![q.to_owned()];
    }

    let mut out = vec![q.to_owned()];
    match request_translations(q).await {
        Ok(extra) => {
            for t in extra {
                let t = t.trim().to_owned();
                // 去重：翻译结果可能和原文相同（专有名词），也可能彼此重复
                if !t.is_empty() && !out.iter().any(|it| it.eq_ignore_ascii_case(&t)) {
                    out.push(t);
                }
            }
        }
        Err(err) => {
            tracing::debug!("translate: 扩展失败，只用原文搜（{err}）");
        }
    }

    // 最多 3 个查询（原文 + 日 + 英）。再多会明显拖慢首屏。
    out.truncate(3);

    {
        let mut cache = CACHE.lock().unwrap();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(q.to_owned(), out.clone());
    }
    out
}

/// 至少包含一个 CJK 字符才认为需要翻译。
fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        // 中日韩统一表意文字 + 假名
        (0x4E00..=0x9FFF).contains(&u) || (0x3040..=0x30FF).contains(&u)
    })
}

/// 向**我们自己的服务端**请求翻译。
///
/// 服务端返回 `{"ja": "...", "en": "..."}`，缺字段就跳过。
async fn request_translations(query: &str) -> anyhow::Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Resp {
        ja: Option<String>,
        en: Option<String>,
    }

    let base = crate::client::HUAMEI_SITE.trim_end_matches('/');
    let resp: Resp = crate::client::recv_raw(
        crate::client::http_client()
            .get(format!("{base}/api/hm/translate"))
            .query(&[("q", query)])
            /* 翻译是锦上添花：3 秒拿不到就放弃，绝不能拖住搜索。
               （全局超时是 20 秒，对搜索来说太久了。） */
            .timeout(std::time::Duration::from_secs(3)),
    )
    .await?
    .json()
    .await?;

    Ok([resp.ja, resp.en].into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_detection() {
        assert!(contains_cjk("无呼吸义务"));
        assert!(contains_cjk("ノンブレス"));
        assert!(!contains_cjk("non-breath oblige"));
        assert!(!contains_cjk("12345"));
    }

    /// 空查询不该产生任何网络请求（expand 的第一行就返回了）。
    #[test]
    fn empty_query_returns_empty() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(rt.block_on(expand("   ")).is_empty());
    }
}
