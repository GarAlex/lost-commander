# fish completion for lost-commander.
# Both binaries take directories, so file completion is narrowed to them.

complete -c lostc -f -a "(__fish_complete_directories)"
complete -c lostc -s h -l help -d "print a summary of the keys and exit"
complete -c lostc -s V -l version -d "print the version and exit"
complete -c lostc -l list -d "print a directory listing and exit"

complete -c lostc-gui -f -a "(__fish_complete_directories)"
complete -c lostc-gui -s h -l help -d "print a summary and exit"
complete -c lostc-gui -s V -l version -d "print the version and exit"
complete -c lostc-gui -l grid -d "start both panes in the icon grid"
complete -c lostc-gui -l tree -d "start with the tree above the files"
complete -c lostc-gui -l preview -d "start the right pane following the left"
complete -c lostc-gui -l screenshot -r -F -d "render a few frames to a PNG and exit"
