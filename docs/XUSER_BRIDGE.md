# BMCBL XUser Bridge

## Activation

The bridge is available only in Win32 Minecraft builds that load `BLoader.dll` as a static import.

BMCBL creates `\\.\pipe\BMCBL.XUser.<minecraft-pid>` before resuming the suspended Win32 process. BLoader attempts to open that exact pipe during process attach.

- Pipe absent: return immediately; do not resolve `xgameruntime.dll`, do not initialize MinHook, and do not alter official Microsoft login.
- Pipe present but validation fails: reject the session and preserve official Microsoft login.
- Valid session: import the short-lived pre-authentication document, install one hook on `xgameruntime.dll!QueryApiImpl`, and intercept only `CLSID_XUserImpl`.

UWP launches do not create the pipe and therefore never activate the bridge.

## Transport validation

The fixed transport header contains:

- protocol magic and version;
- target Minecraft PID;
- launcher PID;
- issue and expiry times;
- bounded payload length;
- SHA-256 payload digest.

BLoader also verifies that the pipe server PID equals the Minecraft parent process. BMCBL verifies the connected pipe client PID equals the newly created Minecraft PID.

No access token, private key, profile identifier, activation flag, or file path is placed in the child environment or command line.

## Hook boundary

Only `QueryApiImpl` is detoured. Requests for runtime classes other than `CLSID_XUserImpl` call the original Microsoft trampoline unchanged. All other XGameRuntime exports are never replaced by BLoader.

## Xbox request signing

`XUserGetTokenAndSignatureAsync` and its UTF-16 variant return both:

- `XBL3.0 x=<user-hash>;<token>` authorization data;
- a Base64 Xbox proof-of-possession Signature.

The signed stream contains policy version, Windows FILETIME timestamp, uppercase HTTP method, absolute request path and query, Authorization value, policy-selected header values, and exact body bytes, with the required NUL separators. BLoader hashes the stream with SHA-256 and signs the digest with the session P-256 device key through Windows BCrypt.

This is required for Presence, friends/activity information, and other Xbox Live title-service requests that reject unsigned proof-of-possession traffic.

## Process boundary

The transport removes credentials from environment variables, command lines, registry values, and temporary files. It cannot protect credentials from malicious code already running inside the Minecraft process with equivalent memory access. For this reason BLoader consumes the session before loading third-party Mods and clears transport and request buffers as soon as they are no longer needed.
