define-command -override mkdr-enable -docstring 'Enable markdown rendering' %{
    # try で囲む: WinSetOption が2回発火した場合に add-highlighter が
    # 「既に存在する」エラーで失敗しないようにする
    try %{ add-highlighter window/mkdr-conceal replace-ranges mkdr_conceal }
    try %{ add-highlighter window/mkdr-faces   ranges         mkdr_faces   }
    hook -group mkdr window NormalIdle .* mkdr-render
    hook -group mkdr window InsertIdle .* mkdr-render
    hook -group mkdr window WinResize   .* mkdr-on-resize
}

define-command -override mkdr-disable -docstring 'Disable markdown rendering' %{
    try %{ remove-highlighter window/mkdr-conceal }
    try %{ remove-highlighter window/mkdr-faces   }
    remove-hooks window mkdr
    # daemon のバッファ状態を解放（メモリリーク防止）
    evaluate-commands %sh{
        mkdr send --close --session "$kak_session" --bufname "$kak_bufname" 2>/dev/null &
    }
}
