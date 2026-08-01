# bash completion for lostc and lostc-gui
#
# Hand-written, because the argument parsing is: there is no clap to generate
# from. Both take directories, so the useful completion is directories plus
# the handful of flags.
_lostc() {
    local cur prev
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    case "$prev" in
        --screenshot)
            # A file to write, so ordinary filename completion.
            COMPREPLY=($(compgen -f -- "$cur"))
            return
            ;;
    esac

    if [[ "$cur" == -* ]]; then
        local flags="--help --version"
        case "${COMP_WORDS[0]##*/}" in
            lostc) flags="$flags --list" ;;
            lostc-gui) flags="$flags --grid --tree --preview --screenshot" ;;
        esac
        COMPREPLY=($(compgen -W "$flags" -- "$cur"))
        return
    fi
    COMPREPLY=($(compgen -d -- "$cur"))
}
complete -o filenames -F _lostc lostc
complete -o filenames -F _lostc lostc-gui
