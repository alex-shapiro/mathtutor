---
name: ash-sandbox
description: Sandbox for restricting file access, network connections, process execution, IO devices, and environment variables based on a policy file. Actions outside the policy are denied by the operating system.
---

You may be running inside an Ash sandbox session. This sandbox will restrict your ability to access files, initiate network connections, execute processes, use IO devices, and access environment variables.

## Read Policy

The default policy is defined at `.ash/policy.yml` relative to the project directory. You have readonly access to this file. Any rule that is not in the file is denied by default.

## Test Permissions with `ash test`

Before attempting an action that may be denied, use `ash test` to check whether it is allowed. A test will avoid wasted work and confusing errors from denied syscalls. If you test with the `--ui` option, any unknown command will prompt the user interactively; this ensures you know exactly what permissions the user wants you to have.

### Overall

```sh
USAGE: ash test <subcommand>

SUBCOMMANDS:
  file                    Test a filesystem action
  network                 Test a network action
  exec                    Test an exec action
  io_device               Test an IO device action
  environment             Test whether an environment variable is allowed in the session

ARGUMENTS:
  --ui                    Prompt interactively when result is unknown
  --policy <policy>       Path to the policy file. If called inside an ash session, default is the current session policy.
                          Otherwise, default is .ash/policy.yml
  --version               Show the version.
  -h, --help              Show help information.
```

#### Test File

```sh
USAGE: ash test file [--policy <policy>] [--ui] <operation> <path>

ARGUMENTS:
  <operation>             File operation (values: read, write, create, delete, rename)
  <path>                  Path to test

Examples:
  ash test file read /tmp/data.txt
  ash test file write /etc/passwd --ui
  ash test file create ~/new-file.txt --policy my-policy.yml
```

#### Test Network

```sh
USAGE: ash test network [--policy <policy>] [--ui] [--direction <direction>] [--transport <transport>] <host>

ARGUMENTS:
  --direction <direction> Connection direction (values: inbound, outbound; default: outbound)
  --transport <transport> Transport protocol (values: tcp, udp; default: tcp)
  <host>                  Hostname or IP address, with optional port: <host>[:<port>]

Examples:
  ash test network example.com:443
  ash test network --direction outbound api.example.com
  ash test network --transport udp dns.example.com:53
```

#### Test Exec

```sh
USAGE: ash test exec [--policy <policy>] [--ui] <command> ...

ARGUMENTS:
  <command>               Command to evaluate, with subcommands and args

Examples:
  ash test exec /bin/ls
  ash test exec --ui "rm -rf /"
  ash test exec --policy my-policy.yml "python3 script.py"
```

#### Test IO Device

```sh
USAGE: ash test io_device [--policy <policy>] [--ui] <device>

ARGUMENTS:
  <device>                Device class to access

Examples:
  ash test io_device IOAccelDevice
  ash test io_device "AppleGPU*" --ui
  ash test io_device IOHIDResourceDeviceUserClient --policy my-policy.yml
```

#### Test ENV

```sh
USAGE: ash test environment [--policy <policy>] [--ui] <variable>

ARGUMENTS:
  <variable>              Environment variable name

Examples:
  ash test environment HOME
  ash test environment SECRET_KEY --ui
  ash test environment PATH --policy my-policy.yml
```
