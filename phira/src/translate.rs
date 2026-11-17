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

/// 启动时预热：把字典读进内存并确保已落盘。
///
/// 不在首次搜索时懒加载的原因：
/// · 搜索是热路径，冷启动那一下会明显卡手
/// · 落盘只在第一次运行发生，顺带让用户能在 data/ 下看到格式
pub fn preload() {
    let _ = local_dict_lookup("__preload__");
}

/// 本地曲名字典的缓存（读到内存后不再重复读盘）。
static LOCAL_DICT: Lazy<Mutex<Option<HashMap<String, Vec<String>>>>> = Lazy::new(|| Mutex::new(None));

/// 查本地字典。命中返回 [原文, 外文原名...]；未命中返回 None（继续走后面的后端）。
fn local_dict_lookup(query: &str) -> Option<Vec<String>> {
    let mut guard = LOCAL_DICT.lock().unwrap();
    if guard.is_none() {
        /* 种子字典**编译进二进制**（include_str!）。
           为什么不用 assets/ 运行时读：iOS 的 app bundle 是只读的，
           用户没法往里放东西；而 data/ 在沙盒深处，用户也摸不到。
           所以把内置的 347 条曲名别名硬编进来，首次运行时释放到 data/，
           之后用户可以自由增删（比如用「从文件导入」替换成自己的字典）。 */
        const SEED: &str = include_str!("../assets/song_aliases.json");

        let path = crate::dir::root().ok().map(|d| format!("{d}/song_aliases.json"));
        let text = match path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(t) => t,
            None => {
                // 首次运行：把内置字典落盘，方便用户查看格式并自行扩充
                if let Some(p) = &path {
                    let _ = std::fs::write(p, SEED);
                }
                SEED.to_owned()
            }
        };
        let dict = serde_json::from_str::<HashMap<String, Vec<String>>>(&text).unwrap_or_default();
        if !dict.is_empty() {
            tracing::info!("translate: 已加载本地曲名字典 {} 条", dict.len());
        }
        *guard = Some(dict);
    }
    let dict = guard.as_ref().unwrap();

    // 精确匹配 → 大小写无关 → 包含匹配（「无呼吸」也能命中「无呼吸义务」）
    let key = query.trim();
    let mut out = vec![key.to_owned()];
    let found = dict
        .get(key)
        .or_else(|| dict.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v))
        .or_else(|| {
            dict.iter()
                .find(|(k, _)| k.contains(key) || key.contains(k.as_str()))
                .map(|(_, v)| v)
        });

    if let Some(aliases) = found {
        for a in aliases {
            let a = a.trim();
            if !a.is_empty() && !out.iter().any(|it| it.eq_ignore_ascii_case(a)) {
                out.push(a.to_owned());
            }
        }
        if out.len() > 1 {
            return Some(out);
        }
    }
    None
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
    /* 优先级 1：**本地字典**（data/song_aliases.json）。
       ─────────────────────────────────────────────────────────
       这是最可靠的路径：纯离线、零延迟、零依赖、可被社区补充。

       格式（简中/繁中 别名 → 外文原名列表）：
         {
           "无呼吸义务": ["ノンブレス・オブリージュ", "Non-breath Oblige"],
           "粉彩线条":   ["Pastel Lines"]
         }
       键是用户可能输入的中文，值是要额外拿去搜的外文原名。

       为什么走字典而不是在线翻译：实测 Google 翻译直连 000、
       VocaDB 被 Cloudflare 拦、GitHub 没有现成的中日曲名对照库。
       而曲名是**专有名词**，机器翻译本来就不准（「无呼吸义务」
       会被译成「呼吸する義務なし」，而真名是片假名外来语）。
       字典反而是唯一正确解。 */
    if let Some(hit) = local_dict_lookup(query) {
        return Ok(hit);
    }

    /* 优先级 2：用**用户自己部署**的翻译服务（设置里的「翻译服务地址」）。
       本地部署的好处：不依赖我们、不受网络限制、没有 key 成本。
       没填才回落到我们服务端的 /api/hm/translate。 */
    if let Some(endpoint) = crate::get_data().config.translate_endpoint.as_deref() {
        let ep = endpoint.trim().trim_end_matches('/');
        if !ep.is_empty() {
            return libretranslate(ep, query).await;
        }
    }
    server_translate(query).await
}

/// LibreTranslate 协议：`POST {endpoint}/translate`
/// 请求 `{"q","source","target","format"}`，响应 `{"translatedText"}`
///
/// 选它是因为它是开源自托管里最常见的一个（docker 一条命令起服务），
/// 而且协议简单到不需要额外依赖 —— 任何能返回同样 JSON 的服务都能用。
async fn libretranslate(endpoint: &str, query: &str) -> anyhow::Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Resp {
        #[serde(rename = "translatedText")]
        translated: Option<String>,
    }

    let client = crate::client::http_client();
    let mut out = Vec::new();
    for target in ["ja", "en"] {
        let res = client
            .post(format!("{endpoint}/translate"))
            .json(&serde_json::json!({
                "q": query,
                "source": "auto",
                "target": target,
                "format": "text",
            }))
            .timeout(std::time::Duration::from_secs(4))
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {
                if let Ok(Resp { translated: Some(t) }) = r.json::<Resp>().await {
                    if !t.trim().is_empty() {
                        out.push(t);
                    }
                }
            }
            Ok(r) => tracing::debug!("translate: 本地服务返回 {}", r.status()),
            Err(err) => tracing::debug!("translate: 本地服务不可达（{err}）"),
        }
    }
    Ok(out)
}

/// 回落：我们自己的服务端代理。
async fn server_translate(query: &str) -> anyhow::Result<Vec<String>> {
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
