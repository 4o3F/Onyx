# Onyx

**Onyx** is a stress-testing tool for DOMjudge pre-flight checks. It helps identify issues early by simulating multiple teams logging in, submitting solutions, and polling key endpoints (e.g., team pages and the scoreboard).

> [!CAUTION]
> This program is still experimental. Use it in production at your own risk.

> [!IMPORTANT]
> **Authorized use only.** Do not run this against any system you do not own or operate, or without explicit written permission.

## Usage

1. Copy the example configuration and adjust it for your environment:
   - See `config.toml.example`

2. Prepare your directory layout. An example structure:

```

.
├── config.toml
├── solutions
│   ├── A
│   │   ├── AC.cpp
│   │   └── TLE.cpp
│   ├── B
│   │   ├── AC.cpp
│   │   └── TLE.cpp
│   └── C
│       ├── AC.cpp
│       └── TLE.cpp
└── team.csv

```

### `team.csv` format

`team.csv` follows the same format as **Natsume**: http://github.com/4o3F/Natsume

It must contain the following columns:

```

id,username,password

```

Example:

```

C22,team001,0d000721

```

> [!TIP]
> The `id` column is currently **not used** by Onyx (it is kept for compatibility).

## Logic

The simulation flow is:

```mermaid
flowchart TD
    A[Start] --> B[LOGIN]
    B --> C[Wait all users logged in]
    C -- wait 0~30s --> D[GET /team]
    D --> E{All problems AC?}
    E -- No, wait 0~10s --> F[Submit one problem AC or TLE]
    F -- wait 0~10s --> G[GET /team/scoreboard]
    G -- wait 0~10s --> H[Random GET /team]
    H -- repeat 0~10 times --> I[GET /team/scoreboard]
    I -- wait 0~20s --> J[Random GET /team]
    J -- repeat 0~5 times --> E
    E -- Yes --> K[Exit user flow]
```
