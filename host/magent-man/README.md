# mAgent-Man

mAgent Device Manager - BLE Configuration Tool for ESP32-C61

## Features

- **BLE Device Discovery**: Scan and connect to mAgent devices via Bluetooth 5.0
- **WiFi Configuration**: Set WiFi SSID and password
- **LLM Configuration**: Configure LLM model and API key
- **System Monitoring**: Real-time status, memory usage, WiFi signal
- **Dark/Light Mode**: Theme toggle with system preference detection
- **Multi-language**: English, 简体中文, 繁體中文
- **Modern UI**: Built with Tailwind CSS

## Quick Start

```bash
# Install dependencies
bun install

# Build Swift BLE Helper
cd ble-helper
swift build -c release
cp ./.build/release/ble-helper ../

# Run development
cd ..
bun run tauri dev

# Build for production
bun run tauri build
```

## Requirements

- [Bun](https://bun.sh) v1.0+
- [Rust](https://rustup.rs) 1.70+
- [Xcode](https://developer.apple.com/xcode/) 14+ (macOS)
- ESP-IDF toolchain (for firmware)

## Project Structure

```
host/magent-man/
├── src/
│   ├── App.tsx              # Main application
│   ├── main.tsx             # Entry point
│   ├── index.css            # Tailwind styles
│   ├── i18n/                # Internationalization
│   │   ├── index.ts         # i18n configuration
│   │   └── locales/         # Translation files
│   │       ├── en.json
│   │       ├── zh.json
│   │       └── zh-TW.json
│   ├── contexts/            # React contexts
│   │   └── ThemeContext.tsx # Theme provider
│   ├── components/          # UI components
│   │   ├── DeviceList.tsx
│   │   ├── ConfigPanel.tsx
│   │   ├── StatusMonitor.tsx
│   │   ├── StatusBar.tsx
│   │   └── SettingsDropdown.tsx
│   ├── hooks/               # React hooks
│   │   └── useBle.ts       # BLE operations
│   └── types/               # TypeScript types
│       └── index.ts
├── src-tauri/               # Tauri/Rust backend
└── ble-helper/              # Swift BLE Helper
```

## BLE GATT UUIDs

| UUID | Name | Description |
|------|------|-------------|
| 0x1850 | Config Service | Main configuration service |
| 0x2A01 | WiFi SSID | Write WiFi network name |
| 0x2A02 | WiFi Password | Write WiFi password |
| 0x2A03 | LLM Model | Write LLM model name |
| 0x2A04 | LLM API Key | Write API key |
| 0x2A05 | Hostname | Write device hostname |
| 0x2A06 | Status | Read/Notify system status |
| 0x2A07 | Device Info | Read device information |
| 0x2A08 | System Commands | Execute AT commands |
| 0x2A09 | System Responses | AT command responses |
| 0x2A0A | WiFi Status | Read/Notify WiFi state |
| 0x2A0B | Conversation Log | Read conversation history |

## License

MIT
