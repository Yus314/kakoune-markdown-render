# ハイライター用（window スコープ）
declare-option -hidden range-specs mkdr_conceal
declare-option -hidden range-specs mkdr_faces

# no-op 判定用キャッシュ（window スコープ: 3要素）
declare-option -hidden str mkdr_last_timestamp   ''
declare-option -hidden str mkdr_last_width        ''
declare-option -hidden str mkdr_last_cursor_line  ''

# パフォーマンス最適化キャッシュ（window スコープ）
# mkdr_daemon_alive: true のとき PING パスで --check-alive をスキップ（2-5ms 節約）
# mkdr_last_config_hash: 最後の RENDER/PING 応答時の "{ts_hex}:{hash_hex}" 複合文字列
#   空文字列 → daemon は即スキップ（RENDER 完了前 / GlobalSetOption リセット後）
declare-option -hidden bool mkdr_daemon_alive     false
declare-option -hidden str  mkdr_last_config_hash ''

# 設定オプション（buffer スコープ、21個）
declare-option -hidden int  mkdr_cursor_context  0
declare-option -hidden str  mkdr_heading_char_1  '▌'
declare-option -hidden str  mkdr_heading_char_2  '▌'
declare-option -hidden str  mkdr_heading_char_3  '▌'
declare-option -hidden str  mkdr_heading_char_4  '▌'
declare-option -hidden str  mkdr_heading_char_5  '▌'
declare-option -hidden str  mkdr_heading_char_6  '▌'
declare-option -hidden bool mkdr_heading_setext  false
declare-option -hidden str  mkdr_thematic_char   '─'
declare-option -hidden str  mkdr_blockquote_char '▎'
declare-option -hidden str  mkdr_bullet_char_1   '•'
declare-option -hidden str  mkdr_bullet_char_2   '◦'
declare-option -hidden str  mkdr_bullet_char_3   '▸'
declare-option -hidden str  mkdr_task_unchecked  '☐'
declare-option -hidden str  mkdr_task_checked    '☑'
declare-option -hidden str  mkdr_code_fence_char '▔'
declare-option -hidden bool mkdr_enable_bold      true
declare-option -hidden bool mkdr_enable_italic    true
declare-option -hidden bool mkdr_enable_code_span true
declare-option -hidden bool mkdr_enable_link      true
declare-option -hidden bool mkdr_enable_table     true
declare-option -hidden str  mkdr_preset           'default'
