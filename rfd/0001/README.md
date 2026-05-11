---
authors: Mason Stallmo <mason@stallmo.com>
state: accepted 
discussion: https://tangled.org/did:plc:aj4au7qecohfi7zlimyxow2r/pulls/11
labels: security, vm, tools, sandbox
---

# RFD 1: Process Isolation for Native Command Execution

## Introduction

Ein uses Wasm (wasmtime) to sandbox tool plugins. The sandbox is effective for pure Wasm operations — WASI preopened directories, host-mediated network access — but runs into a roadblock when needing to execute native binaries on the system. Since wasmtime, and WASI in general, does not provide a sandboxed way to spawn a subrpocess to enable agents to execute CLIs (cargo, bun, jj, etc.) the `eind` host provides the `spawn` syscall to execute a native process. The spawned process runs as a regular OS process with full access to the host filesystem and network, bypassing every constraint the Wasm sandbox provides.

This RFD defines a microVM-based isolation layer that ensures spawned processes inherit the same `allowed_paths` and `allowed_hosts` constraints that govern the Wasm sandbox. The design must provide equivalent security guarantees on all supported platforms.

## Supported Platforms

- **macOS:** Apple Silicon (ARM64) only. Intel Macs are out of scope. Apple completed the Apple Silicon transition in 2022 and the remaining Intel Mac install base does not justify the engineering cost of a separate virtualization backend.
- **Linux:** x86_64 and ARM64.

## Problem Statement

### Threat Model

Ein targets two adversaries:

1. **Malicious tool authors.** A community-published Wasm tool claims to perform one function (e.g., formatting Markdown) but internally invokes the `spawn` syscall to exfiltrate user data. The user has no visibility into what the tool actually does at the native process level and no enforcement mechanism to prevent it.

2. **Prompt injection.** A malicious prompt manipulates the LLM into using the Bash tool (or any tool with process execution access) to run commands that exfiltrate data, modify files outside the working directory, or establish outbound network connections to unauthorized hosts.

### Current State

Ein provides `allowed_paths` and `allowed_hosts` configuration at both the global and per-plugin level. These constraints are enforced by wasmtime's WASI implementation for filesystem and network operations that stay within Wasm. The enforcement model relies on the fact that Wasm guests can only interact with the outside world through host-provided imports, and those imports contain the policy logic.

The `spawn` syscall in `ein_tool` breaks this model. It hands a raw command string to the host, which executes it as a native subprocess via `bash -c`. That subprocess:

- Has full access to the host filesystem (not scoped to `allowed_paths`)
- Has full network access (not scoped to `allowed_hosts`)
- Can spawn further subprocesses with the same unrestricted access
- Is invisible to the Wasm sandbox — wasmtime has no knowledge of or control over it

### Alternatives Considered

| Approach | Rejection Reason |
|---|---|
| **Command string parsing / allowlists** | Shell syntax is too expressive for reliable static analysis. Pipes, subshells, command substitution, backticks, and encoding tricks create unbounded bypass opportunities. |
| **Landlock (Linux LSM)** | Linux-only. No equivalent on macOS. Would create a platform security asymmetry that violates Ein's goal of universal, equivalent guarantees. |
| **Seatbelt / sandbox_init (macOS)** | Undocumented, deprecated by Apple, and fragile. Building a security-critical system on an API Apple is actively moving away from is not viable. |
| **ptrace-based syscall filtering** | On macOS: SIP restrictions prevent tracing system binaries in `/usr/bin`, Apple Silicon requires debugger entitlements, and performance overhead is significant for build workloads (millions of syscalls). |
| **eBPF / seccomp / network namespaces** | Linux-only kernel features with no macOS equivalent. |
| **FUSE-based virtual filesystem** | Covers filesystem but not network. macFUSE has its own stability and licensing issues. Adds significant complexity for partial coverage. |
| **Removing process execution entirely** | Too restrictive. Real-world agent workflows require running build tools (`cargo`, `npm`, `make`), version control (`git`), and other native binaries that cannot be replaced with pure Wasm equivalents. |
| **WASI native subprocess support** | No WASI proposal for subprocess spawning exists on any concrete roadmap. The WASI subgroup has acknowledged the need (GitHub issue #414) but stated it depends on Component Model dynamic instantiation, which is not expected in the near future. WASIX (Wasmer) provides `exec`/`wait` but is a non-standard, single-runtime extension that does not address sandboxing. |

### Determination

The only mechanism that provides equivalent, kernel-enforced isolation on both macOS and Linux is hardware virtualization. macOS provides Hypervisor.framework (HVF); Linux provides KVM. Both support the same virtio device model for filesystem and network I/O. A lightweight microVM running a minimal Linux kernel can apply Landlock, network namespace, and seccomp restrictions uniformly on both platforms — the guest Linux kernel provides the enforcement layer, and the host hypervisor provides the isolation boundary.

## Proposed Design

### Overview

Process execution requests from Wasm tools are routed into a per-session microVM. The microVM runs a minimal Linux kernel with an init process that accepts commands over virtio-vsock, executes them under Landlock and network namespace constraints derived from the tool's policy, and returns stdout/stderr/exit code over the same channel.

```
┌──────────────────────────────────────────────────────────────┐
│                          eind                                │
│                                                              │
│  ┌──────────────┐         ┌────────────────────────────────┐ │
│  │  Wasm Tool   │         │        Process VM              │ │
│  │  (ein_bash)  │         │                                │ │
│  │              │ spawn   │  ┌──────────────────────────┐  │ │
│  │  Calls ──────┼────────►│  │   Minimal Linux Kernel   │  │ │
│  │  spawn()     │         │  │                          │  │ │
│  │              │         │  │  ┌────────────────────┐  │  │ │
│  └──────────────┘         │  │  │   ein_vm_agent     │  │  │ │
│                           │  │  │                    │  │  │ │
│  ┌──────────────┐         │  │  │  • Landlock fs     │  │  │ │
│  │  Wasm Tool   │         │  │  │    policy          │  │  │ │
│  │  (ein_read)  │         │  │  │  • Network ns      │  │  │ │
│  │              │         │  │  │    policy          │  │  │ │
│  │  No spawn ───┼── X     │  │  │  • seccomp         │  │  │ │
│  │  access      │         │  │  │    filters         │  │  │ │
│  └──────────────┘         │  │  └────────────────────┘  │  │ │
│                           │  │                          │  │ │
│  ┌──────────────┐         │  │  virtio-fs: allowed_paths│  │ │
│  │  Policy      │────────►│  │  virtio-vsock: cmd chan  │  │ │
│  │  Engine      │         │  │  virtio-net: filtered    │  │ │
│  │              │         │  └──────────────────────────┘  │ │
│  │  config.json │         └────────────────────────────────┘ │
│  └──────────────┘                                            │
└──────────────────────────────────────────────────────────────┘
```

### `ein_vm` — MicroVM Manager

*New crate: `crates/ein_vm/`*

Manages the lifecycle of the process execution VM. Responsible for:

- **VM boot.** Configures and starts the microVM using libkrun. libkrun provides a C API that abstracts over KVM (Linux) and Hypervisor.framework (macOS/ARM64), making it the only production-grade, cross-platform microVM library available. The VM runs a minimal Linux kernel (provided by libkrunfw) with a custom initramfs containing the `ein_vm_agent` binary.

- **Filesystem mounting.** Exposes only the directories listed in the tool's merged `allowed_paths` (global + per-plugin) into the VM via virtio-fs. Directories not in the allow list are physically absent from the VM's filesystem — there is no path traversal or escape possible because the files do not exist in the guest.

- **Network configuration.** Uses libkrun's TSI (Transparent Socket Impersonation) or virtio-net with a host-side proxy to restrict outbound connections to the `allowed_hosts` for the invoking tool. If `allowed_hosts` is empty, no network interface or socket forwarding is configured — the VM has no network connectivity.

- **Command dispatch.** Sends process execution requests to `ein_vm_agent` over virtio-vsock and receives results (stdout, stderr, exit code) over the same channel.

- **VM lifecycle.** The VM boots on first process execution request in a session and remains running for the session's duration. Subsequent requests reuse the running VM. The VM is shut down when the session ends.

### `ein_vm_agent` — In-VM Execution Agent

*New crate: `crates/ein_vm_agent/`*

A statically-linked Linux binary (compiled for the guest architecture) that runs as PID 1 (init) inside the microVM. Responsible for:

- **Command execution.** Listens on virtio-vsock for execution requests. Each request contains a command string and a policy descriptor. Spawns the command via `bash -c` inside a restricted execution context.

- **Landlock enforcement.** Before executing each command, applies a Landlock ruleset scoped to the paths that were mounted into the VM. This provides defense-in-depth: even if a virtio-fs mount were somehow misconfigured, Landlock prevents access to anything outside the policy. Landlock is always available because the guest always runs a Linux kernel, regardless of the host OS.

- **Network namespace enforcement.** Each command execution runs inside a network namespace. If the tool's policy specifies `allowed_hosts`, a minimal network namespace is configured with a proxy that forwards only to those hosts. If no hosts are allowed, the namespace has no network interfaces.

- **seccomp filtering.** Applies a seccomp-bpf filter to the spawned process to block syscalls that could be used for sandbox escape (e.g., `mount`, `reboot`, `kexec_load`, `ptrace`).

- **Resource limits.** Applies `setrlimit` constraints (CPU time, file size, number of processes) to prevent fork bombs and resource exhaustion.

- **Result streaming.** Streams stdout and stderr back to the host over virtio-vsock as the command runs, and sends the exit code on completion.

### Modifications to `eind`

#### `src/tools/syscalls.rs`

The `spawn` host function implementation changes from directly executing `bash -c <command>` to routing the request through `ein_vm`:

```
Current:   spawn(command) → bash -c command → stdout/stderr
Proposed:  spawn(command, tool_id) → ein_vm::execute(command, policy) → stdout/stderr
```

The host looks up the invoking tool's identity (filename stem, e.g., `ein_bash`), retrieves its merged policy (`allowed_paths` + `allowed_hosts` from global config and `plugin_configs`), and passes both the command and the policy to the VM manager.

#### `src/tools.rs`

`WasmToolSet` gains a reference to the `ein_vm` manager. When instantiating each tool's Wasm component, the tool's identity is threaded through to the syscall handler so that `spawn` calls can be attributed to the correct tool and its policy applied.

#### Session Lifecycle

The VM manager is initialized when a session starts (on `SessionConfig` receipt) but the VM itself boots lazily on first `spawn` call. This means sessions that never use process execution (e.g., read/write-only tools) pay no VM overhead. The VM is shut down when the gRPC session stream closes after all in-process work is completed.

`config_update` messages that change `allowed_paths` or `allowed_hosts` for a tool that has already triggered VM boot require careful handling. Options:

- **Restart the VM** with updated mounts/network. Simple but loses any in-VM state.
- **Apply incrementally** by sending updated Landlock/network rules to the agent. More complex but avoids restart.

Recommendation: restart the VM on policy-affecting config changes. Config changes are infrequent, and the VM boots in sub-second time, so the disruption is minimal.

### Guest Image

The guest environment has two layers: a boot layer managed by Ein and a rootfs layer defined by the user.

#### Boot Layer (initramfs)

A minimal initramfs containing:

- **`ein_vm_agent`** (statically linked) — runs as PID 1 (init). Handles the vsock command protocol, applies Landlock/seccomp/namespace restrictions, and supervises command execution.
- **A minimal init script** that mounts the OCI rootfs, sets up overlayfs, pivots root into the user-provided environment, and then execs `ein_vm_agent` within that environment.

The initramfs is built as part of Ein's release process and embedded in the `eind` binary (via `include_bytes!`). It is not user-configurable — it is Ein's trusted computing base inside the VM.

#### Kernel

Provided by `libkrunfw`, a companion project to libkrun that ships a pre-built, minimal Linux kernel optimized for microVM boot time. Approximately 3-5 MB compressed. Also embedded in or shipped alongside `eind`.

#### User Rootfs (OCI Image)

The execution environment — toolchains, utilities, libraries — is defined as an OCI container image. This is the layer users interact with. The boot sequence is:

1. Kernel boots with the initramfs
2. `ein_vm_agent` starts as PID 1
3. Agent mounts the OCI rootfs layers (extracted on the host and exposed to the VM via virtio-fs)
4. Agent sets up an overlayfs: OCI rootfs as the lower (read-only) layer, a tmpfs as the upper (writable) layer
5. Agent pivots root into the overlayfs
6. Agent mounts the user's `allowed_paths` into the rootfs at designated mount points via virtio-fs
7. Agent begins listening on vsock for execution requests

This separation means `ein_vm_agent` is always Ein's code, injected at the boot layer, regardless of what the user's rootfs contains. A user cannot accidentally or intentionally replace the agent.

#### Image Configuration

Users configure their VM environment in `config.json` using one of three methods, in order of precedence:

**1. Default (zero config).** Ein ships a default OCI image (`ghcr.io/mstallmo/ein-base:latest`) containing common developer toolchains: Rust, Node.js, Python, git, and standard coreutils. This covers the majority of use cases with no setup.

```json
{
  "vm": {}
}
```

**2. Image reference.** User specifies a pre-built OCI image to pull from a registry. Good for teams sharing a standardized environment.

```json
{
  "vm": {
    "image": "ghcr.io/myorg/ein-dev:latest"
  }
}
```

**3. Dockerfile.** User provides a path to a Dockerfile. Ein builds it into an OCI image and caches the result. Good for project-specific environments.

```json
{
  "vm": {
    "dockerfile": "~/.ein/Dockerfile"
  }
}
```

Or relative to the project directory:

```json
{
  "vm": {
    "dockerfile": "./ein.Dockerfile"
  }
}
```

#### Image Build and Cache

When a Dockerfile is specified, Ein builds it using a daemonless OCI builder (e.g., `buildah` or an embedded build library) — Docker Desktop is not required. The build enforces `--platform linux/arm64` on macOS ARM64 hosts and the appropriate architecture on Linux hosts to ensure the image contains binaries that can execute inside the VM's Linux guest.

Built images are cached at `~/.ein/images/`. Ein rebuilds only when the Dockerfile's content hash changes. Pre-built images specified by reference are pulled and cached on first use, then reused until the user explicitly requests an update.

The OCI image layers are extracted into a flat rootfs directory on the host. This directory is exposed to the VM as a read-only virtio-fs mount, which `ein_vm_agent` uses as the lower layer of the overlayfs. This avoids needing any OCI tooling inside the VM itself.

#### Architecture Matching

The OCI image must contain binaries matching the guest architecture:

- **macOS ARM64 host → ARM64 Linux guest.** Images must be `linux/arm64`.
- **Linux x86_64 host → x86_64 Linux guest.** Images must be `linux/amd64`.
- **Linux ARM64 host → ARM64 Linux guest.** Images must be `linux/arm64`.

Ein validates the image architecture at build/pull time and rejects mismatches with a clear error message.

On Linux hosts, an alternative to the OCI rootfs is available: host-installed tools can be mounted directly into the VM via virtio-fs, since they are already Linux ELF binaries. This can be configured alongside or instead of an OCI image for users who prefer to use their host toolchain directly (see Open Questions, item 7).

### Communication Protocol

Communication between `ein_vm` (host) and `ein_vm_agent` (guest) uses virtio-vsock with a simple JSON-over-newline protocol:

#### Host → Guest: Execution Request

```json
{
  "id": "req-001",
  "command": "cargo build --release",
  "working_dir": "/workspace",
  "env": {
    "CARGO_HOME": "/workspace/.cargo",
    "PATH": "/usr/local/bin:/usr/bin:/bin"
  },
  "policy": {
    "allowed_paths": [
      {"host": "/home/user/project", "guest": "/workspace", "writable": true}
    ],
    "allowed_hosts": ["crates.io", "index.crates.io", "static.crates.io"],
    "max_cpu_seconds": 300,
    "max_file_size_bytes": 104857600,
    "max_processes": 64
  }
}
```

#### Guest → Host: Output Stream

```json
{"id": "req-001", "type": "stdout", "data": "   Compiling ein v0.1.0\n"}
{"id": "req-001", "type": "stderr", "data": "warning: unused variable\n"}
{"id": "req-001", "type": "exit", "code": 0}
```

#### Guest → Host: Error

```json
{"id": "req-001", "type": "error", "message": "policy violation: path /etc/passwd not in allowed_paths"}
```

### Security Properties

This design provides the following guarantees:

1. **Filesystem isolation.** A spawned process can only access directories explicitly listed in the tool's `allowed_paths`. This is enforced at two levels: the virtio-fs mount (files outside `allowed_paths` are not present in the VM) and Landlock (defense-in-depth against mount misconfiguration).

2. **Network isolation.** A spawned process can only establish outbound connections to hosts listed in the tool's `allowed_hosts`. This is enforced by the VM's network configuration — either TSI filtering or a host-side proxy that rejects connections to unauthorized destinations. If `allowed_hosts` is empty, the VM has no network connectivity.

3. **Platform equivalence.** The same Linux kernel runs inside the VM on both macOS and Linux hosts. Landlock, network namespaces, and seccomp are kernel features of the guest, not the host, so they are uniformly available regardless of host OS.

4. **Tool attribution.** Every process execution request is associated with the tool that initiated it. The host applies the correct per-tool policy. A tool cannot escalate its privileges by impersonating another tool because tool identity is determined by the host based on which Wasm component invoked the syscall, not by anything the guest controls.

5. **No ambient authority.** The Wasm tool interface does not change. Tools that do not invoke `spawn` are unaffected. Tools that do invoke `spawn` get the same interface they have today, but the process runs inside the VM. Tool authors do not need to be aware of the VM — the isolation is transparent.

## Open Questions

1. **libkrun Rust bindings maturity.** libkrun exposes a C API. The quality and completeness of existing Rust binding crates needs evaluation. It may be necessary to write and maintain bindings in-tree.

2. **Architecture-specific guest images.** If Ein supports both x86_64 and ARM64 Linux hosts, the guest initramfs must contain binaries for the correct architecture. This may require shipping multiple guest images or building architecture-specific packages.

3. **Boot time budget.** Sub-second boot is achievable with libkrun but needs measurement with the full init sequence (Landlock setup, vsock listener, virtio-fs mounts). If boot time exceeds the budget, a VM pool or pre-boot strategy may be needed.

4. **Config update semantics.** The current recommendation is to restart the VM on policy-changing config updates. This needs validation that it doesn't disrupt user workflows (e.g., a long-running build in progress when config changes).

5. **virtiofs performance for build workloads.** `cargo build` and similar tools perform heavy, random-access file I/O. virtiofs performance relative to native filesystem access needs benchmarking to ensure build times are acceptable.

6. **Default image scope and maintenance.** The `ein-base` default image needs to include enough toolchains to cover the majority of users out of the box. The proposed set (Rust, Node, Python, Go, git, coreutils) covers most developer agent workflows, but the image size and update cadence need consideration. A large image increases first-run pull time; a small image increases the likelihood users need a custom Dockerfile. The default image also needs a maintenance strategy — regular rebuilds to pick up security patches and toolchain updates. On Linux hosts, users can alternatively mount host toolchains directly into the VM via virtio-fs, bypassing the OCI rootfs for tools entirely. This option should be documented but is not required — the OCI image path works uniformly on both platforms.

7. **Uniform VM vs. platform-specific backends.** The current design uses the microVM uniformly on both platforms, which guarantees identical security semantics. The OCI image model significantly reduces the macOS toolchain friction that previously motivated a platform-specific backend approach, since users can define their environment with a Dockerfile. However, an alternative architecture would use Landlock + network namespaces + seccomp on Linux (native process execution, host tools available, zero overhead) and the microVM only on macOS. The tradeoff is implementation complexity (two backends) vs. user experience (no VM overhead on Linux). This decision is deferred pending performance benchmarking of the VM path on Linux.

8. **Daemonless OCI builder selection.** `buildah` is the most mature daemonless OCI builder but is a Go binary and an external dependency. An embedded Rust-based alternative would reduce dependencies but is significantly more engineering effort. A third option is to only support pre-built image references (no Dockerfile builds) in the initial release, and add Dockerfile build support later.

## Dependencies

### libkrun

- Repository: [github.com/containers/libkrun](https://github.com/containers/libkrun)
- License: Apache 2.0
- Platforms: Linux (KVM), macOS ARM64 (Hypervisor.framework). Intel Macs are not supported.
- Integration: C API, linked as a dynamic library. Rust bindings to be written in `ein_vm` or sourced from community crates if available.

### libkrunfw

- Companion to libkrun. Provides a pre-built minimal Linux kernel for microVM use. Eliminates the need to maintain a custom kernel build.

### Guest Toolchain

- `ein_vm_agent` must be compiled as a static Linux binary targeting the guest architecture (`aarch64-unknown-linux-musl` or `x86_64-unknown-linux-musl`). It is embedded in the initramfs, not in the OCI rootfs.
- The OCI rootfs provides the user-visible toolchain (Rust, Node, etc.). On macOS ARM64 hosts, images must target `linux/arm64`. On Linux hosts, images must match the host architecture.
- On Linux hosts, users have the additional option of mounting host-installed tools into the VM via virtio-fs instead of (or alongside) an OCI rootfs, since host binaries are already Linux ELF.

### OCI Image Builder

- Ein requires a daemonless OCI image builder for Dockerfile support. Candidates: `buildah` (mature, widely used, no daemon required) or an embedded Rust-based builder.
- Docker Desktop is explicitly **not** a dependency. Users should not need Docker installed to use Ein.
- The builder must support `--platform` targeting to produce images for the correct guest architecture.
