# @konnect-xai/konnect

Konnect is a KiCad MCP server. This launcher selects the native server binary for
Linux x64 or ARM64, macOS Intel or Apple Silicon, or Windows x64. Node.js 18 or
later and the package's optional dependencies are required. Install KiCad separately.

Check the installed server version.

```sh
npx -y @konnect-xai/konnect --version
```

An MCP client can use command `npx` with arguments `["-y", "@konnect-xai/konnect"]`.
Pin a specific version by appending `@<version>` to the package name.

For the KiCad plugin, download the PCM ZIP for your operating system.
See the [Konnect-xai repository](https://github.com/takurot/Konnect-xai) and
[Releases](https://github.com/takurot/Konnect-xai/releases).
