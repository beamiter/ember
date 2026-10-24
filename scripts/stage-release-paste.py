from pathlib import Path
import subprocess
p=Path('src/main.rs')
s=p.read_text()
assert p.read_bytes()==subprocess.check_output(['git','show','387ec628ecab655819be3b571666f070673590a1:src/main.rs'])
old='''        let saw_semantic_paste = events_copy.iter().any(|event| {
            if let egui::Event::Paste(_content) = event {
                crate::debug_log!(
                    "[EVENT] detected Paste event: {:?}",
                    if _content.is_empty() {
                        "empty"
                    } else {
                        "has content"
                    }
                );
                true
            } else {
                false
            }
        });'''
new='''        let saw_semantic_paste = events_copy
            .iter()
            .find_map(|event| match event {
                egui::Event::Paste(content) => Some(content),
                _ => None,
            })
            .inspect(|_content| {
                crate::debug_log!(
                    "[EVENT] detected Paste event: {:?}",
                    if _content.is_empty() {
                        "empty"
                    } else {
                        "has content"
                    }
                );
            })
            .is_some();'''
assert s.count(old)==1
p.write_text(s.replace(old,new))
