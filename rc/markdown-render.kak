source "%val{source_directory}/options.kak"
source "%val{source_directory}/faces.kak"
source "%val{source_directory}/commands.kak"

hook global WinSetOption filetype=markdown %{
    mkdr-enable
    hook -once -always window WinSetOption filetype=(?!markdown).* %{
        mkdr-disable
    }
}

# 幅変化時は強制 RENDER（thematic/code fence が幅依存のため）
# mkdr_last_timestamp も '' にリセットして ts_same=0 → RENDER パスを強制する。
define-command -override -hidden mkdr-on-resize %{
    set-option window mkdr_last_width     ''
    set-option window mkdr_last_timestamp ''
    mkdr-render
}

define-command -override -hidden mkdr-render %{
    evaluate-commands %sh{
        # ---- no-op 判定（常に必要な変数のみ参照・export）----
        # Kakoune は %sh{} 内で明示参照した変数のみを shell に export する。
        # PING パスでは 3オプションのみ参照し、21個の config オプションは RENDER パスにのみ記述する。
        # shellcheck disable=SC2034
        : "${kak_opt_mkdr_cursor_context}" \
          "${kak_opt_mkdr_daemon_alive}" \
          "${kak_opt_mkdr_last_config_hash}"

        ts="$kak_timestamp"
        # kak_window_width は Kakoune 2021.11.08 以降で利用可能。
        # 古い Kakoune / 未定義の場合は 80 にフォールバック（clap の parse エラーを防止）。
        w="${kak_window_width:-80}"
        cl="$kak_cursor_line"
        last_ts="$kak_opt_mkdr_last_timestamp"
        last_w="$kak_opt_mkdr_last_width"
        last_cl="$kak_opt_mkdr_last_cursor_line"
        ctx="$kak_opt_mkdr_cursor_context"

        ts_same=0; w_same=0; cl_same=0
        [ "$ts" = "$last_ts" ] && ts_same=1
        [ "$w"  = "$last_w"  ] && w_same=1
        [ "$cl" = "$last_cl" ] && cl_same=1

        if [ "$ts_same" = "1" ] && [ "$w_same" = "1" ]; then
            if [ "$ctx" = "0" ] || [ "$cl_same" = "1" ]; then
                # 3要素全て変化なし → 完全 no-op（~0ms）
                exit 0
            fi
        fi

        # ---- IPC 処理 ----
        cmd_fifo="$kak_command_fifo"

        if [ "$ts_same" = "1" ]; then
            # ---- PING パス: カーソル or 幅変化のみ ----
            # mkdr_daemon_alive=true のとき --check-alive をスキップ（2-5ms 節約）。
            if [ "$kak_opt_mkdr_daemon_alive" != "true" ]; then
                # daemon がまだ起動していない状態で PING は意味がない。
                # mkdr_last_timestamp をリセットして次の Idle で RENDER を強制する。
                printf 'set-option window mkdr_last_timestamp ""\n'
                printf 'set-option window mkdr_last_cursor_line %s\n' "$cl"
                exit 0
            fi
            mkdr send --ping \
                --session "$kak_session" --bufname "$kak_bufname" \
                --timestamp "$ts" --cursor "$cl" --width "$w" \
                --client "$kak_client" --cmd-fifo "$cmd_fifo" \
                --config-hash "$kak_opt_mkdr_last_config_hash" \
                >/dev/null 2>&1 &
        else
            # ---- RENDER パス: バッファ変更 ----
            # 21個の config オプションをここで参照（RENDER 時のみ export が必要）。
            # shellcheck disable=SC2034
            : "${kak_opt_mkdr_heading_char_1}" "${kak_opt_mkdr_heading_char_2}" \
              "${kak_opt_mkdr_heading_char_3}" "${kak_opt_mkdr_heading_char_4}" \
              "${kak_opt_mkdr_heading_char_5}" "${kak_opt_mkdr_heading_char_6}" \
              "${kak_opt_mkdr_thematic_char}"  "${kak_opt_mkdr_blockquote_char}" \
              "${kak_opt_mkdr_bullet_char_1}"  "${kak_opt_mkdr_bullet_char_2}" \
              "${kak_opt_mkdr_bullet_char_3}"  "${kak_opt_mkdr_task_unchecked}" \
              "${kak_opt_mkdr_task_checked}"   "${kak_opt_mkdr_code_fence_char}" \
              "${kak_opt_mkdr_enable_bold}"    "${kak_opt_mkdr_enable_italic}" \
              "${kak_opt_mkdr_enable_code_span}" "${kak_opt_mkdr_enable_link}" \
              "${kak_opt_mkdr_enable_table}"   "${kak_opt_mkdr_preset}" \
              "${kak_opt_mkdr_cursor_context}"

            # daemon 起動確認: RENDER パスは常に --check-alive（daemon 死亡検知・復旧）。
            if ! mkdr send --check-alive --session "$kak_session" 2>/dev/null; then
                mkdr daemon --session "$kak_session" >/dev/null 2>&1 &
                # ソケット作成を待つ（最大0.5秒）
                # 注: sleep 0.05 は GNU coreutils 依存（BSD では sleep 1 等に調整が必要）
                for _ in 1 2 3 4 5 6 7 8 9 10; do
                    mkdr send --check-alive --session "$kak_session" 2>/dev/null && break
                    sleep 0.05
                done
            fi
            # daemon 生存を window にキャッシュ（次の PING パスで --check-alive をスキップ）
            printf 'set-option window mkdr_daemon_alive true\n'

            response_fifo="$kak_response_fifo"
            # kak_response_fifo はシングルクォートを含まないため sed エスケープ不要。
            printf "eval -no-hooks write '%s'\n" "$response_fifo" > "$cmd_fifo"
            (
                trap - INT QUIT
                mkdr send \
                    --session "$kak_session" --bufname "$kak_bufname" \
                    --timestamp "$ts" --cursor "$cl" --width "$w" \
                    --client "$kak_client" --cmd-fifo "$cmd_fifo" \
                    < "$response_fifo" >/dev/null 2>&1
            ) >/dev/null 2>&1 &
            # mkdr_last_timestamp / mkdr_last_width / mkdr_last_config_hash は
            # daemon の kak -p 応答（format_commands 出力）で設定される（非オプティミスティック）。
        fi
        # カーソル行キャッシュは常に更新（PING/RENDER 問わず）
        printf 'set-option window mkdr_last_cursor_line %s\n' "$cl"
    }
}

# 設定変更時: キャッシュをリセットして次回 RENDER を強制
hook global GlobalSetOption mkdr_.* %{
    set-option window mkdr_last_timestamp    ''
    set-option window mkdr_last_width        ''
    set-option window mkdr_last_config_hash  ''
}

# バッファクローズ時に CLOSE（daemon のバッファ状態を解放してメモリリーク防止）
hook global BufClose .* %sh{
    [ "$kak_opt_filetype" = "markdown" ] || exit 0
    mkdr send --close --session "$kak_session" --bufname "$kak_bufname" 2>/dev/null &
}

# セッション終了時に SHUTDOWN
hook global KakEnd .* %sh{
    mkdr send --shutdown --session "$kak_session" 2>/dev/null
}
