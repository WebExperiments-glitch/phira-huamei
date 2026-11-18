//! 用 phira 自己的解析器（prpr）验证 PEC 谱面 —— 与游戏内走完全相同的代码路径。
//!
//! 用法：cargo run -p pecheck -- <chart.pec>
fn main() {
    let path = match std::env::args().nth(1) {
        Some(v) => v,
        None => {
            eprintln!("usage: pecheck <chart.pec>");
            std::process::exit(2);
        }
    };
    let src = std::fs::read_to_string(&path).expect("读取失败");
    println!("文件 {path}  {} 字节", src.len());
    match prpr::parse::parse_pec(&src, Default::default()) {
        Ok(c) => {
            println!("✅ 解析成功：{} 条判定线，offset={}", c.lines.len(), c.offset);
            for (i, l) in c.lines.iter().enumerate() {
                let holds = l
                    .notes
                    .iter()
                    .filter(|n| matches!(n.kind, prpr::core::NoteKind::Hold { .. }))
                    .count();
                println!(
                    "   line {i}: {} 音符（hold {}），height keyframes={}",
                    l.notes.len(),
                    holds,
                    l.height.keyframes.len()
                );
            }
        }
        Err(e) => {
            println!("❌ 解析失败：{e:?}");
            std::process::exit(1);
        }
    }
}
