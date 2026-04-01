#!/bin/bash
# memfs-init.sh — source this file to enable MemFS virtual filesystem
# Usage: source memfs-init.sh

MEMFS_MOUNT="${MEMFS_MOUNT:-/memories}"
MEMFS_BIN="${MEMFS_BIN:-memfs}"
MEMFS_STATE="${MEMFS_STATE:-$HOME/.memfs_cwd}"

# --- Helpers ---

# Check if a resolved path is inside the virtual FS
_memfs_is_virtual() {
    [[ "$1" == "$MEMFS_MOUNT" || "$1" == "$MEMFS_MOUNT/"* ]]
}

# Check if we're currently inside the virtual FS
_memfs_in_vfs() {
    [[ -f "$MEMFS_STATE" ]] && [[ -s "$MEMFS_STATE" ]]
}

# Get current virtual CWD
_memfs_vcwd() {
    if _memfs_in_vfs; then
        cat "$MEMFS_STATE"
    fi
}

# Resolve a path argument to an absolute path.
# If relative and in vFS, resolve against virtual CWD.
# If absolute, use directly.
_memfs_resolve() {
    local arg="$1"
    if [[ "$arg" == /* ]]; then
        echo "$arg"
    elif _memfs_in_vfs; then
        local vcwd
        vcwd=$(_memfs_vcwd)
        echo "${vcwd}/${arg}"
    else
        echo "$(builtin pwd)/${arg}"
    fi
}

# --- Command Overrides ---

cd() {
    if [[ $# -eq 0 ]]; then
        # bare cd: go home, exit vFS
        if _memfs_in_vfs; then
            : > "$MEMFS_STATE"
        fi
        builtin cd
        return
    fi

    local target="$1"

    # Handle special cases that always exit vFS
    if [[ "$target" == "~" || "$target" == "/" ]]; then
        if _memfs_in_vfs; then
            : > "$MEMFS_STATE"
        fi
        builtin cd "$@"
        return
    fi

    # Expand ~ prefix
    if [[ "$target" == "~/"* ]]; then
        target="$HOME/${target#\~/}"
    fi

    local resolved
    resolved=$(_memfs_resolve "$target")

    if _memfs_is_virtual "$resolved"; then
        "$MEMFS_BIN" cd "$resolved"
    else
        # Exiting vFS or navigating real FS
        if _memfs_in_vfs; then
            : > "$MEMFS_STATE"
        fi
        builtin cd "$@"
    fi
}

ls() {
    local args=() paths=() virtual=false

    for arg in "$@"; do
        if [[ "$arg" == -* ]]; then
            args+=("$arg")
        else
            local resolved
            resolved=$(_memfs_resolve "$arg")
            if _memfs_is_virtual "$resolved"; then
                virtual=true
                paths+=("$resolved")
            else
                paths+=("$arg")
            fi
        fi
    done

    # If no path given and we're in vFS, route to memfs
    if [[ ${#paths[@]} -eq 0 ]] && _memfs_in_vfs; then
        virtual=true
    fi

    if $virtual; then
        "$MEMFS_BIN" ls "${args[@]}" "${paths[@]}"
    else
        command ls "${args[@]}" "${paths[@]}"
    fi
}

pwd() {
    if _memfs_in_vfs; then
        "$MEMFS_BIN" pwd
    else
        builtin pwd "$@"
    fi
}

cat() {
    local args=() files=() virtual=false

    for arg in "$@"; do
        if [[ "$arg" == -* ]]; then
            args+=("$arg")
        else
            local resolved
            resolved=$(_memfs_resolve "$arg")
            if _memfs_is_virtual "$resolved"; then
                virtual=true
                files+=("$arg")
            else
                files+=("$arg")
            fi
        fi
    done

    # If in vFS and files don't have absolute paths, route to memfs
    if _memfs_in_vfs && [[ ${#files[@]} -gt 0 ]]; then
        local any_real=false
        for f in "${files[@]}"; do
            if [[ "$f" == /* ]] && ! _memfs_is_virtual "$f"; then
                any_real=true
                break
            fi
        done
        if ! $any_real; then
            virtual=true
        fi
    fi

    if $virtual; then
        "$MEMFS_BIN" cat "${files[@]}"
    else
        command cat "${args[@]}" "${files[@]}"
    fi
}

mkdir() {
    local args=() paths=() virtual=false

    for arg in "$@"; do
        if [[ "$arg" == -* ]]; then
            args+=("$arg")
        else
            local resolved
            resolved=$(_memfs_resolve "$arg")
            if _memfs_is_virtual "$resolved"; then
                virtual=true
                paths+=("$resolved")
            else
                paths+=("$arg")
            fi
        fi
    done

    if $virtual; then
        "$MEMFS_BIN" mkdir "${args[@]}" "${paths[@]}"
    else
        command mkdir "${args[@]}" "${paths[@]}"
    fi
}

rm() {
    local args=() targets=() virtual=false

    for arg in "$@"; do
        if [[ "$arg" == -* ]]; then
            args+=("$arg")
        else
            local resolved
            resolved=$(_memfs_resolve "$arg")
            if _memfs_is_virtual "$resolved"; then
                virtual=true
                targets+=("$resolved")
            else
                targets+=("$arg")
            fi
        fi
    done

    # If in vFS and target is a bare filename
    if ! $virtual && _memfs_in_vfs && [[ ${#targets[@]} -gt 0 ]]; then
        virtual=true
    fi

    if $virtual; then
        "$MEMFS_BIN" rm "${args[@]}" "${targets[@]}"
    else
        command rm "${args[@]}" "${targets[@]}"
    fi
}

mv() {
    if [[ $# -lt 2 ]]; then
        command mv "$@"
        return
    fi

    local src="${@:1:$#-1}"
    local dst="${@: -1}"
    local src_resolved dst_resolved

    src_resolved=$(_memfs_resolve "$src")
    dst_resolved=$(_memfs_resolve "$dst")

    if _memfs_is_virtual "$src_resolved" || _memfs_is_virtual "$dst_resolved"; then
        "$MEMFS_BIN" mv "$src_resolved" "$dst_resolved"
    else
        command mv "$@"
    fi
}

cp() {
    if [[ $# -lt 2 ]]; then
        command cp "$@"
        return
    fi

    local src="${@:1:$#-1}"
    local dst="${@: -1}"
    local src_resolved dst_resolved

    src_resolved=$(_memfs_resolve "$src")
    dst_resolved=$(_memfs_resolve "$dst")

    if _memfs_is_virtual "$src_resolved" || _memfs_is_virtual "$dst_resolved"; then
        "$MEMFS_BIN" cp "$src_resolved" "$dst_resolved"
    else
        command cp "$@"
    fi
}

grep() {
    local args=() pattern="" paths=() virtual=false has_pattern=false

    for arg in "$@"; do
        if [[ "$arg" == -* ]]; then
            args+=("$arg")
        elif ! $has_pattern; then
            pattern="$arg"
            has_pattern=true
        else
            local resolved
            resolved=$(_memfs_resolve "$arg")
            if _memfs_is_virtual "$resolved"; then
                virtual=true
                paths+=("$resolved")
            else
                paths+=("$arg")
            fi
        fi
    done

    # If no path given and in vFS, search current scope
    if [[ ${#paths[@]} -eq 0 ]] && _memfs_in_vfs; then
        virtual=true
    fi

    if $virtual; then
        "$MEMFS_BIN" grep "${args[@]}" "$pattern" "${paths[@]}"
    else
        command grep "${args[@]}" "$pattern" "${paths[@]}"
    fi
}

find() {
    local path="" args=() virtual=false

    # find's first non-flag argument is the path
    for arg in "$@"; do
        if [[ -z "$path" && "$arg" != -* ]]; then
            local resolved
            resolved=$(_memfs_resolve "$arg")
            if _memfs_is_virtual "$resolved"; then
                virtual=true
                path="$resolved"
            else
                path="$arg"
            fi
        else
            args+=("$arg")
        fi
    done

    # If no path and in vFS
    if [[ -z "$path" ]] && _memfs_in_vfs; then
        virtual=true
        path=$(_memfs_vcwd)
    fi

    if $virtual; then
        "$MEMFS_BIN" find "$path" "${args[@]}"
    else
        if [[ -n "$path" ]]; then
            command find "$path" "${args[@]}"
        else
            command find "${args[@]}"
        fi
    fi
}

# --- Write convenience function ---
# Since bash > and >> redirections can't be intercepted,
# provide a 'write' function for creating/appending memories.
# Usage:
#   write filename.md "content"
#   write filename.md <<< "content"
#   write filename.md << EOF
#   multi-line content
#   EOF
#   echo "content" | write filename.md

write() {
    local filename="$1"
    shift

    if [[ -z "$filename" ]]; then
        echo "memfs: write: missing filename" >&2
        return 1
    fi

    local content
    if [[ $# -gt 0 ]]; then
        content="$*"
    elif [[ ! -t 0 ]]; then
        content=$(command cat)
    else
        echo "memfs: write: no content provided" >&2
        return 1
    fi

    local resolved
    resolved=$(_memfs_resolve "$filename")

    if _memfs_is_virtual "$resolved" || _memfs_in_vfs; then
        "$MEMFS_BIN" write "$filename" "$content"
    else
        printf '%s' "$content" > "$filename"
    fi
}

# Append convenience function
append() {
    local filename="$1"
    shift

    if [[ -z "$filename" ]]; then
        echo "memfs: append: missing filename" >&2
        return 1
    fi

    local content
    if [[ $# -gt 0 ]]; then
        content="$*"
    elif [[ ! -t 0 ]]; then
        content=$(command cat)
    else
        echo "memfs: append: no content provided" >&2
        return 1
    fi

    local resolved
    resolved=$(_memfs_resolve "$filename")

    if _memfs_is_virtual "$resolved" || _memfs_in_vfs; then
        "$MEMFS_BIN" append "$filename" "$content"
    else
        printf '%s' "$content" >> "$filename"
    fi
}
