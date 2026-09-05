# alcatraz-macos

macOS backend that generates a Seatbelt profile and invokes the trusted
`/usr/bin/sandbox-exec` system binary through the `alcatraz` multicall
entry point. Managed network restrictions are represented in the Seatbelt
profile and coordinated with Chaos's proxy.
