# Zen Manual

**Version:** Draft v1
**Status:** Work In Progress

---

# Introduction

Zen is a scripting language, interactive shell, workflow engine, and automation runtime designed around:

* Simple command-oriented syntax
* Structured data pipelines
* Workspace-aware automation
* Permission-controlled execution
* Persistent session state
* Plugin extensibility
* Workflow orchestration

Zen can be used as:

* Interactive REPL
* Script runner
* Workflow engine
* Command orchestration platform
* Plugin host

---

# Getting Started

## Start REPL

```bash
zen
```

or

```bash
zen repl
```

You will see:

```text
zen>
```

Exit using:

```text
.
```

or

```text
:quit
```

or

```text
:exit
```

---

## Run Script

```bash
zen run script.fg
```

---

## Execute Inline

```bash
zen echo "Hello World"
```

---

# Variables

Create variables:

```zen
let name = "Robert"
let age = 50
```

Use variables:

```zen
echo name
```

Variable expansion:

```zen
echo $name
```

---

# Data Types

## String

```zen
"hello"
```

## Number

```zen
123
45.67
```

## Boolean

```zen
true
false
```

## Null

```zen
null
```

## List

```zen
[
  1,
  2,
  3
]
```

## Object

```zen
{
  name: "Robert",
  age: 50
}
```

---

# Expressions

## Arithmetic

```zen
1 + 2
10 - 3
5 * 7
20 / 4
```

Supported operators:

```text
+
-
*
/
```

---

## Comparison

```zen
10 > 5
10 >= 5
10 < 20
10 <= 20
10 == 10
10 != 5
```

---

## Logical

```zen
true && true
true || false
!false
```

---

# Flow Control

## If Statement

```zen
if age >= 18 {
    echo "Adult"
}
```

```zen
if age >= 18 {
    echo "Adult"
} else {
    echo "Minor"
}
```

---

## Try / Catch / Finally

```zen
try {
    dangerous.operation
}
catch err {
    echo err
}
finally {
    echo "Cleanup"
}
```

---

# Pipelines

Zen supports structured data pipelines.

Example:

```zen
users
| where age > 18
| select name, age
```

---

## Select

```zen
users
| select name, age
```

---

## Fields

```zen
users
| fields name, age
```

---

## Get

```zen
user
| get name
```

---

## Sort

```zen
users
| sort age
```

Descending:

```zen
users
| sort age desc
```

---

## Limit

```zen
users
| limit 10
```

---

## Count

```zen
users
| count
```

---

## Sum

```zen
orders
| sum total
```

---

## Avg

```zen
orders
| avg total
```

---

## Max

```zen
orders
| max total
```

---

## Min

```zen
orders
| min total
```

---

## Distinct

```zen
users
| distinct country
```

---

## Table Output

```zen
users
| table
```

---

## JSON

Convert to JSON:

```zen
users
| to-json
```

Convert from JSON:

```zen
jsonText
| from-json
```

---

## Save Output

```zen
requires {
    fs.write
}

users
| to-json
| save users.json
```

---

# Permissions

Zen uses explicit permissions.

Example:

```zen
requires {
    proc.exec
    fs.read
}
```

The REPL will prompt before granting permissions.

## Common Permissions

```text
proc.exec
proc.read

fs.read
fs.write

workspace.read
workspace.env

state.read
state.write
```

---

# Workspace Commands

## Workspace Root

```zen
workspace.root
```

Returns the workspace root directory.

---

## Current Working Directory

```zen
workspace.cwd
```

Returns the current directory.

---

## Read File

```zen
workspace.read "README.md"
```

---

## Check File Exists

```zen
workspace.exists "README.md"
```

---

## Find Files

```zen
workspace.find "*.md"
```

---

## List Files

```zen
workspace.files
```

---

## List Directories

```zen
workspace.dirs
```

---

## Environment Variable

```zen
workspace.env "PATH"
```

---

# State Commands

State commands persist session variables.

## Save State

```zen
state.save
```

---

## Load State

```zen
state.load
```

---

## Clear Saved State

```zen
state.clear
```

Deletes the saved state file.

Does not modify current session variables.

---

## List Session Variables

```zen
state.list
```

Example output:

```json
[
  {
    "name": "server",
    "value": "prod"
  }
]
```

---

# Shell Commands

## Change Directory

```zen
cd "C:/temp"
```

---

## Print Current Directory

```zen
pwd
```

---

## Locate Command

```zen
which cargo
```

---

## Clear Screen

```zen
clear
```

---

# Process Commands

## Execute Process

```zen
requires {
    proc.exec
}

exec cargo --version
```

---

## List Processes

```zen
proc.list
```

---

# Plugin System

Zen supports built-in and external plugins.

Plugin directory:

```text
.zen/plugins
```

---

## Discover Plugins

```zen
plugins.discover
```

---

## Load Plugin

```zen
plugins.load "plugin.toml"
```

---

## Unload Plugin

```zen
plugins.unload "plugin-name"
```

---

## Reload Plugins

```zen
plugins.reload
```

---

# REPL Commands

```text
:help
:commands
:clear
:history
:plugins
:permissions
:doctor
:reset
:startup
:reload
:load PATH
:save PATH
:vars
```

---

# Startup Files

Zen loads startup files automatically.

## Global Startup

```text
<config>/zen/startup.fg
```

## Workspace Startup

```text
.zen/startup.fg
```

Useful for:

* Variable initialization
* Plugin loading
* Environment setup
* Common aliases

---

# Workflows

Zen contains a workflow runtime supporting:

* Workflow steps
* Conditions
* Retry policies
* Checkpoints
* Rollback handlers
* Failure handlers
* Finally handlers
* Persistence
* Resume support

Workflow documentation will be expanded in a future edition.

---

# Troubleshooting

## state.save Returns Access Denied

Example:

```text
Failed to create state directory: Access is denied. (os error 5)
```

State files are saved relative to the workspace root:

```text
<workspace>/.zen/state.json
```

Check:

```zen
workspace.root
workspace.cwd
```

to verify where Zen is attempting to save state.

---

# Roadmap

Future manual sections:

* Workflow Reference
* Plugin SDK
* External Plugin Manifest Format
* PostgreSQL Plugin
* Dropbox Plugin
* Notification Centre
* Agent Runtime
* Time Functions
* Audit System
* Workspace Architecture
* Certo Integration

---

End of Manual
