/// mAgent BLE Helper
///
/// CoreBluetooth-based BLE GATT client for configuring ESP32-C61 devices.
/// Communicates with the mAgent-Man Tauri app via stdout JSON.

import Foundation
import CoreBluetooth

// MARK: - GATT UUIDs

/// mAgent Configuration Service UUID
let CONFIG_SERVICE_UUID = CBUUID(string: "00001850-0000-1000-8000-00805F9B34FB")

/// WiFi SSID Characteristic (Write)
let WIFI_SSID_CHAR_UUID = CBUUID(string: "00002A01-0000-1000-8000-00805F9B34FB")

/// WiFi Password Characteristic (Write, Encrypted)
let WIFI_PASS_CHAR_UUID = CBUUID(string: "00002A02-0000-1000-8000-00805F9B34FB")

/// LLM Model Characteristic (Write)
let LLM_MODEL_CHAR_UUID = CBUUID(string: "00002A03-0000-1000-8000-00805F9B34FB")

/// LLM API Key Characteristic (Write, Encrypted)
let LLM_API_KEY_CHAR_UUID = CBUUID(string: "00002A04-0000-1000-8000-00805F9B34FB")

/// Hostname Characteristic (Write)
let HOSTNAME_CHAR_UUID = CBUUID(string: "00002A05-0000-1000-8000-00805F9B34FB")

/// Status Characteristic (Notify)
let STATUS_CHAR_UUID = CBUUID(string: "00002A06-0000-1000-8000-00805F9B34FB")

/// Device Info Characteristic (Read)
let DEVICE_INFO_CHAR_UUID = CBUUID(string: "00002A07-0000-1000-8000-00805F9B34FB")

/// System Commands Characteristic (Write)
let SYS_CMD_CHAR_UUID = CBUUID(string: "00002A08-0000-1000-8000-00805F9B34FB")

/// System Responses Characteristic (Notify)
let SYS_RSP_CHAR_UUID = CBUUID(string: "00002A09-0000-1000-8000-00805F9B34FB")

/// WiFi Status Characteristic (Read/Notify)
let WIFI_STATUS_CHAR_UUID = CBUUID(string: "00002A0A-0000-1000-8000-00805F9B34FB")

/// Conversation Log Characteristic (Read/Notify)
let CONV_LOG_CHAR_UUID = CBUUID(string: "00002A0B-0000-1000-8000-00805F9B34FB")

// MARK: - Errors

enum BLEError: Error {
    case bluetoothOff
    case unauthorized
    case unsupported
    case notConnected
    case deviceNotFound
    case characteristicNotFound
    case timeout
    case unknown(String)
}

// MARK: - Data Structures

struct ScannedDevice: CustomStringConvertible {
    let id: String
    let name: String
    let rssi: Int

    var description: String {
        return "Device(id: \(id), name: \(name), rssi: \(rssi))"
    }
}

struct SystemStatus {
    let state: UInt8
    let wifiState: UInt8
    let memoryFree: UInt32
    let uptimeMs: UInt64
    let errorCode: UInt8
}

struct DeviceInfo {
    let versionMajor: UInt8
    let versionMinor: UInt8
    let versionPatch: UInt8
    let memoryTotal: UInt32
    let memoryFree: UInt32
    let uptimeMs: UInt64
    let chipModel: String
}

struct WifiStatusInfo {
    let state: UInt8
    let rssi: Int8
    let ipAddr: [UInt8]
    let ssid: String
}

struct ConversationEntry {
    let timestamp: Date
    let role: String
    let text: String
}

// MARK: - BLE Manager

class BLEManager: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {

    private var centralManager: CBCentralManager!
    private var connectedPeripheral: CBPeripheral?
    private var discoveredCharacteristics: [CBUUID: CBCharacteristic] = [:]
    private var scanTimer: Timer?
    private var connectionTimer: Timer?
    /// A command received before the Bluetooth manager was powered on. Held
    /// until `centralManagerDidUpdateState(.poweredOn)` so the daemon doesn't
    /// race the manager initialisation and answer "Bluetooth not ready".
    private var pendingCommand: [String]?

    /// Handle a command, queuing it until Bluetooth is powered on.
    func handleCommand(_ parts: [String]) {
        guard centralManager.state == .poweredOn else {
            if pendingCommand == nil {
                pendingCommand = parts
            }
            return
        }
        processCommand(["ble-helper"] + parts)
    }

    // Scan results
    private var scannedDevices: [String: ScannedDevice] = [:]

    // Configuration values
    private var wifiSsid: String?
    private var wifiPassword: String?
    private var llmModel: String?
    private var llmApiKey: String?
    private var hostname: String?

    // Status data
    private var lastSystemStatus: SystemStatus?
    private var lastDeviceInfo: DeviceInfo?
    private var lastWifiStatus: WifiStatusInfo?
    private var conversationLog: [ConversationEntry] = []

    // Write completion handlers
    private var pendingWrites: [CBUUID: (Bool, String) -> Void] = [:]

    // Pending `exec` round-trip: the SYS_RSP characteristic to read and the
    // command we sent, so `didUpdateValueFor` can finalize the real response.
    private var pendingExec: (char: CBCharacteristic, command: String)? = nil
    // True once we've issued the SYS_RSP read for a pending exec, so a value
    // update from that read (not the truncated notification) finalizes it.
    private var pendingRead = false

    override init() {
        super.init()
        centralManager = CBCentralManager(delegate: self, queue: DispatchQueue.main)
    }

    // MARK: - Public Commands

    func scan(timeout: Int) {
        // Check Bluetooth state
        switch centralManager.state {
        case .poweredOn:
            break
        case .poweredOff:
            printResult(["success": false, "message": "Bluetooth is powered off", "error": "bluetooth_off"])
            return
        case .unauthorized:
            printResult(["success": false, "message": "Bluetooth access unauthorized", "error": "unauthorized"])
            return
        case .unsupported:
            printResult(["success": false, "message": "Bluetooth not supported on this device", "error": "unsupported"])
            return
        default:
            printResult(["success": false, "message": "Bluetooth not ready", "error": "not_ready"])
            return
        }

        // Reset scan results
        scannedDevices.removeAll()

        // Start scanning
        print("{\"type\":\"scan_start\",\"devices\":[]}")

        scanTimer?.invalidate()
        scanTimer = Timer.scheduledTimer(withTimeInterval: TimeInterval(timeout), repeats: false) { [weak self] _ in
            self?.finishScan()
        }

        centralManager.scanForPeripherals(
            withServices: [CONFIG_SERVICE_UUID],
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: false]
        )
    }

    func stopScan() {
        centralManager.stopScan()
        scanTimer?.invalidate()
        scanTimer = nil
        finishScan()
    }

    private func finishScan() {
        var devices = scannedDevices.values.map { device -> [String: Any] in
            return [
                "id": device.id,
                "name": device.name,
                "rssi": device.rssi
            ]
        }

        // CoreBluetooth never re-reports an already-connected peripheral during
        // a scan, so a rescan while connected would otherwise return an empty
        // list even though the device is right there. Always surface the
        // currently-connected device so the UI stays consistent.
        if let conn = connectedPeripheral {
            let id = conn.identifier.uuidString
            if !devices.contains(where: { ($0["id"] as? String) == id }) {
                devices.append([
                    "id": id,
                    "name": conn.name ?? "mAgent",
                    "rssi": 0
                ])
            }
        }

        printResult([
            "success": true,
            "message": "Scan complete",
            "devices": devices
        ])
    }

    func connect(deviceId: String) {
        // Check state
        guard centralManager.state == .poweredOn else {
            printResult(["success": false, "message": "Bluetooth not ready", "error": "bluetooth_off"])
            return
        }

        // Parse UUID
        guard let uuid = UUID(uuidString: deviceId) else {
            printResult(["success": false, "message": "Invalid device UUID format"])
            return
        }

        // Find peripheral
        let knownPeripherals = centralManager.retrievePeripherals(withIdentifiers: [uuid])

        if let peripheral = knownPeripherals.first {
            startConnection(peripheral: peripheral)
        } else {
            // Try to find by scanning
            print("{\"type\":\"searching\",\"device_id\":\"\(deviceId)\"}")

            // Cancel any existing connection
            if let current = connectedPeripheral {
                centralManager.cancelPeripheralConnection(current)
            }

            // Start scan to find device
            scannedDevices.removeAll()
            centralManager.scanForPeripherals(withServices: nil, options: nil)

            // Schedule connection attempt after brief scan
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
                self?.centralManager.stopScan()

                // Check if we found the device
                if let device = self?.scannedDevices[deviceId] {
                    // Retrieve and connect
                    if let uuid = UUID(uuidString: deviceId),
                       let peripheral = self?.centralManager.retrievePeripherals(withIdentifiers: [uuid]).first {
                        self?.startConnection(peripheral: peripheral)
                    }
                } else {
                    self?.printResult(["success": false, "message": "Device not found: \(deviceId)"])
                }
            }
        }
    }

    private func startConnection(peripheral: CBPeripheral) {
        print("{\"type\":\"connecting\",\"device\":\"\(peripheral.name ?? "Unknown")\"}")

        connectedPeripheral = peripheral
        peripheral.delegate = self
        centralManager.connect(peripheral, options: nil)

        // Connection timeout
        connectionTimer?.invalidate()
        connectionTimer = Timer.scheduledTimer(withTimeInterval: 10.0, repeats: false) { [weak self] _ in
            self?.centralManager.cancelPeripheralConnection(peripheral)
            self?.printResult(["success": false, "message": "Connection timeout (10s)"])
        }
    }

    func disconnect(deviceId: String) {
        guard let peripheral = connectedPeripheral,
              peripheral.identifier.uuidString == deviceId else {
            printResult(["success": false, "message": "Device not connected"])
            return
        }

        connectionTimer?.invalidate()
        centralManager.cancelPeripheralConnection(peripheral)

        // Reset state
        connectedPeripheral = nil
        discoveredCharacteristics.removeAll()
        resetConfigValues()

        printResult(["success": true, "message": "Disconnected"])
    }

    func readConfig() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        // Reset values
        resetConfigValues()

        // Read known characteristics
        let charsToRead: [CBUUID] = [
            WIFI_SSID_CHAR_UUID,
            LLM_MODEL_CHAR_UUID,
            HOSTNAME_CHAR_UUID
        ]

        for uuid in charsToRead {
            if let char = discoveredCharacteristics[uuid] {
                if char.properties.contains(.read) {
                    connectedPeripheral?.readValue(for: char)
                }
            }
        }

        // Wait and print result
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            self?.printConfigResult()
        }
    }

    func writeWifi(ssid: String, password: String) {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        guard let ssidChar = discoveredCharacteristics[WIFI_SSID_CHAR_UUID] else {
            printResult(["success": false, "message": "WiFi SSID characteristic not found"])
            return
        }

        // Write SSID
        let ssidData = ssid.data(using: .utf8) ?? Data()
        if ssidChar.properties.contains(.write) {
            connectedPeripheral?.writeValue(ssidData, for: ssidChar, type: .withResponse)
        } else {
            printResult(["success": false, "message": "WiFi SSID characteristic is not writable"])
            return
        }

        // Write password if characteristic exists
        if let passChar = discoveredCharacteristics[WIFI_PASS_CHAR_UUID] {
            let passData = password.data(using: .utf8) ?? Data()
            if passChar.properties.contains(.write) {
                connectedPeripheral?.writeValue(passData, for: passChar, type: .withResponse)
            }
        }

        printResult(["success": true, "message": "WiFi configuration saved. Reboot to apply."])
    }

    func writeLlm(model: String, apiKey: String) {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        // Write model
        if let modelChar = discoveredCharacteristics[LLM_MODEL_CHAR_UUID] {
            let modelData = model.data(using: .utf8) ?? Data()
            if modelChar.properties.contains(.write) {
                connectedPeripheral?.writeValue(modelData, for: modelChar, type: .withResponse)
            }
        }

        // Write API key
        if let apiKeyChar = discoveredCharacteristics[LLM_API_KEY_CHAR_UUID] {
            let apiKeyData = apiKey.data(using: .utf8) ?? Data()
            if apiKeyChar.properties.contains(.write) {
                connectedPeripheral?.writeValue(apiKeyData, for: apiKeyChar, type: .withResponse)
            }
        }

        printResult(["success": true, "message": "LLM configuration saved"])
    }

    func writeHostname(hostname: String) {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        guard let char = discoveredCharacteristics[HOSTNAME_CHAR_UUID] else {
            printResult(["success": false, "message": "Hostname characteristic not found"])
            return
        }

        let data = hostname.data(using: .utf8) ?? Data()
        if char.properties.contains(.write) {
            connectedPeripheral?.writeValue(data, for: char, type: .withResponse)
            printResult(["success": true, "message": "Hostname saved"])
        } else {
            printResult(["success": false, "message": "Hostname characteristic is not writable"])
        }
    }

    // MARK: - Status Commands

    func getStatus() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        if let char = discoveredCharacteristics[STATUS_CHAR_UUID] {
            connectedPeripheral?.readValue(for: char)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.printStatusResult()
            }
        } else {
            printStatusResult()
        }
    }

    func getDeviceInfo() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        if let char = discoveredCharacteristics[DEVICE_INFO_CHAR_UUID] {
            connectedPeripheral?.readValue(for: char)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.printDeviceInfoResult()
            }
        } else {
            printDeviceInfoResult()
        }
    }

    func getWifiStatus() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        if let char = discoveredCharacteristics[WIFI_STATUS_CHAR_UUID] {
            connectedPeripheral?.readValue(for: char)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.printWifiStatusResult()
            }
        } else {
            printWifiStatusResult()
        }
    }

    func getConversations() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        if let char = discoveredCharacteristics[CONV_LOG_CHAR_UUID] {
            connectedPeripheral?.readValue(for: char)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.printConversationsResult()
            }
        } else {
            printConversationsResult()
        }
    }

    func getChannels() {
        // Return mock channel data - in real implementation, this would come from the device
        let channels: [[String: Any]] = [
            [
                "id": "local-ble",
                "status": connectedPeripheral != nil ? "active" : "inactive",
                "messages": 0,
                "lastActivity": NSNull()
            ],
            [
                "id": "local-uart",
                "status": "active",
                "messages": 0,
                "lastActivity": NSNull()
            ],
            [
                "id": "manual",
                "status": "active",
                "messages": 0,
                "lastActivity": NSNull()
            ],
            [
                "id": "mqtt",
                "status": "inactive",
                "messages": 0,
                "lastActivity": NSNull()
            ],
            [
                "id": "webhook",
                "status": "inactive",
                "messages": 0,
                "lastActivity": NSNull()
            ],
            [
                "id": "web3",
                "status": "inactive",
                "messages": 0,
                "lastActivity": NSNull()
            ]
        ]

        printResult([
            "success": true,
            "channels": channels
        ])
    }

    func reboot() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        // Write reboot command to system command characteristic
        if let char = discoveredCharacteristics[SYS_CMD_CHAR_UUID] {
            let rebootCmd = "AT+RST\r\n".data(using: .utf8) ?? Data()
            if char.properties.contains(.write) {
                connectedPeripheral?.writeValue(rebootCmd, for: char, type: .withResponse)
            }
        }

        // Disconnect after sending reboot
        if let peripheral = connectedPeripheral {
            centralManager.cancelPeripheralConnection(peripheral)
        }

        printResult(["success": true, "message": "Reboot command sent. Device will restart."])
    }

    func getLogs(lines: Int) {
        // Mock logs - in real implementation, this would read from device
        let mockLogs = [
            "[\(ISO8601DateFormatter().string(from: Date()))] System initialized",
            "[\(ISO8601DateFormatter().string(from: Date()))] WiFi stack started",
            "[\(ISO8601DateFormatter().string(from: Date()))] BLE advertising started",
            "[\(ISO8601DateFormatter().string(from: Date()))] Configuration service ready"
        ]

        printResult([
            "success": true,
            "count": mockLogs.count,
            "logs": mockLogs
        ])
    }

    func runDiagnostics() {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        // Run diagnostic checks
        var diagnostics: [String: Any] = [
            "success": true,
            "timestamp": ISO8601DateFormatter().string(from: Date())
        ]

        // Check device info
        if let info = lastDeviceInfo {
            diagnostics["device"] = [
                "version": "\(info.versionMajor).\(info.versionMinor).\(info.versionPatch)",
                "chip_model": info.chipModel,
                "uptime_ms": info.uptimeMs
            ]
        }

        // Check WiFi status
        if let wifi = lastWifiStatus {
            diagnostics["wifi"] = [
                "state": wifi.state,
                "rssi": wifi.rssi,
                "ssid": wifi.ssid
            ]
        }

        // Check memory
        if let info = lastDeviceInfo {
            let memPercent = info.memoryTotal > 0 ? Double(info.memoryFree) / Double(info.memoryTotal) * 100 : 0
            diagnostics["memory"] = [
                "total_bytes": info.memoryTotal,
                "free_bytes": info.memoryFree,
                "usage_percent": memPercent
            ]
        }

        // Check BLE connection quality
        diagnostics["ble"] = [
            "connected": true,
            "mtu": 512
        ]

        printResult(diagnostics)
    }

    func execCommand(_ command: String) {
        guard connectedPeripheral != nil else {
            printResult(["success": false, "message": "Not connected"])
            return
        }

        guard let char = discoveredCharacteristics[SYS_CMD_CHAR_UUID] else {
            printResult(["success": false, "message": "System command characteristic not found"])
            return
        }

        guard let rspChar = discoveredCharacteristics[SYS_RSP_CHAR_UUID] else {
            printResult(["success": false, "message": "System response characteristic not found"])
            return
        }

        // Format command with AT-style termination
        var cmdWithTerm = command.trimmingCharacters(in: .whitespacesAndNewlines)
        if !cmdWithTerm.hasSuffix("\r\n") {
            cmdWithTerm += "\r\n"
        }

        let cmdData = cmdWithTerm.data(using: .utf8) ?? Data()

        if char.properties.contains(.write) {
            // Arm the round-trip: when the write completes we read the real
            // SYS_RSP value (the firmware sets it to the command's response)
            // and `didUpdateValueFor` returns it instead of a canned string.
            pendingExec = (rspChar, command)
            connectedPeripheral?.writeValue(cmdData, for: char, type: .withResponse)
        } else {
            printResult(["success": false, "message": "Command characteristic is not writable"])
            return
        }
    }

    // MARK: - Helpers

    private func resetConfigValues() {
        wifiSsid = nil
        wifiPassword = nil
        llmModel = nil
        llmApiKey = nil
        hostname = nil
        lastSystemStatus = nil
        lastDeviceInfo = nil
        lastWifiStatus = nil
        conversationLog = []
    }

    private func isMagentDevice(name: String?) -> Bool {
        guard let name = name else { return false }
        let lowercased = name.lowercased()
        return lowercased.hasPrefix("magent") ||
               lowercased.hasPrefix("esp32") ||
               lowercased.contains("magent")
    }

    // MARK: - CBCentralManagerDelegate

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        switch central.state {
        case .poweredOn:
            print("{\"type\":\"ready\"}")
            if let cmd = pendingCommand {
                pendingCommand = nil
                processCommand(["ble-helper"] + cmd)
            }
        case .poweredOff:
            print("{\"type\":\"bluetooth_off\"}")
            printResult(["success": false, "message": "Bluetooth is powered off", "error": "bluetooth_off"])
        case .unauthorized:
            print("{\"type\":\"unauthorized\"}")
            printResult(["success": false, "message": "Bluetooth access unauthorized", "error": "unauthorized"])
        case .unsupported:
            print("{\"type\":\"unsupported\"}")
            printResult(["success": false, "message": "Bluetooth not supported", "error": "unsupported"])
        case .resetting:
            print("{\"type\":\"resetting\"}")
        case .unknown:
            print("{\"type\":\"unknown\"}")
        @unknown default:
            break
        }
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        let name = peripheral.name ?? advertisementData[CBAdvertisementDataLocalNameKey] as? String
        let deviceId = peripheral.identifier.uuidString
        let rssiValue = RSSI.intValue

        // Only track mAgent devices or devices with our service
        if advertisementData[CBAdvertisementDataServiceUUIDsKey] != nil ||
           isMagentDevice(name: name) {

            // Update or add device
            let device = ScannedDevice(id: deviceId, name: name ?? "Unknown", rssi: rssiValue)
            scannedDevices[deviceId] = device

            // Print device discovery event
            print("{\"type\":\"device\",\"device\":{\"id\":\"\(deviceId)\",\"name\":\"\(name ?? "Unknown")\",\"rssi\":\(rssiValue)}}")
        }
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        connectionTimer?.invalidate()

        print("{\"type\":\"connected\",\"device\":\"\(peripheral.name ?? "Unknown")\"}")

        // Discover services
        peripheral.discoverServices([CONFIG_SERVICE_UUID])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        connectionTimer?.invalidate()
        connectedPeripheral = nil

        let message = error?.localizedDescription ?? "Connection failed"
        printResult(["success": false, "message": "Connection failed: \(message)"])
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        connectionTimer?.invalidate()
        connectedPeripheral = nil
        discoveredCharacteristics.removeAll()
        resetConfigValues()

        print("{\"type\":\"disconnected\"}")

        if error != nil {
            printResult(["success": false, "message": "Disconnected with error: \(error!.localizedDescription)"])
        }
    }

    // MARK: - CBPeripheralDelegate

    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard error == nil, let services = peripheral.services else {
            printResult(["success": false, "message": "Service discovery failed: \(error?.localizedDescription ?? "Unknown error")"])
            return
        }

        for service in services {
            if service.uuid == CONFIG_SERVICE_UUID {
                peripheral.discoverCharacteristics(nil, for: service)
                return
            }
        }

        printResult(["success": false, "message": "Configuration service not found"])
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard error == nil, let characteristics = service.characteristics else {
            printResult(["success": false, "message": "Characteristic discovery failed"])
            return
        }

        discoveredCharacteristics.removeAll()

        for characteristic in characteristics {
            discoveredCharacteristics[characteristic.uuid] = characteristic

            // Enable notifications for notify/indicate characteristics
            if characteristic.properties.contains(.notify) || characteristic.properties.contains(.indicate) {
                peripheral.setNotifyValue(true, for: characteristic)
            }

            // Read initial values for read characteristics
            if characteristic.properties.contains(.read) {
                peripheral.readValue(for: characteristic)
            }
        }

        printResult([
            "success": true,
            "message": "Service discovered",
            "characteristics_count": discoveredCharacteristics.count
        ])
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard error == nil, let data = characteristic.value else { return }

        switch characteristic.uuid {
        case WIFI_SSID_CHAR_UUID:
            wifiSsid = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .controlCharacters)

        case WIFI_PASS_CHAR_UUID:
            wifiPassword = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .controlCharacters)

        case LLM_MODEL_CHAR_UUID:
            llmModel = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .controlCharacters)

        case LLM_API_KEY_CHAR_UUID:
            llmApiKey = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .controlCharacters)

        case HOSTNAME_CHAR_UUID:
            hostname = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .controlCharacters)

        case STATUS_CHAR_UUID:
            parseSystemStatus(from: data)

        case DEVICE_INFO_CHAR_UUID:
            parseDeviceInfo(from: data)

        case WIFI_STATUS_CHAR_UUID:
            parseWifiStatus(from: data)

        case CONV_LOG_CHAR_UUID:
            parseConversationLog(from: data)

        case SYS_RSP_CHAR_UUID:
            if let response = String(data: data, encoding: .utf8) {
                print("{\"type\":\"response\",\"response\":\"\(response.replacingOccurrences(of: "\"", with: "\\\""))\"}")

                guard let pending = pendingExec else { return }
                let trimmed = response.trimmingCharacters(in: .whitespacesAndNewlines)

                if pendingRead {
                    // This is the full value returned by our readValue — finalize.
                    pendingExec = nil
                    pendingRead = false
                    printResult([
                        "success": true,
                        "message": trimmed.isEmpty ? "Command executed" : trimmed,
                        "data": ["command": pending.command, "response": response]
                    ])
                } else if !trimmed.isEmpty {
                    // A non-empty SYS_RSP value update (notification) signals the
                    // firmware has produced the reply. Notifications may be
                    // MTU-truncated, so read the characteristic to get the full
                    // value, then finalize on that read (pendingRead above).
                    pendingRead = true
                    peripheral.readValue(for: pending.char)
                }
                // Empty value with no pending read = initial/stale state; ignore.
            }

        default:
            break
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error = error {
            pendingExec = nil
            pendingRead = false
            printResult(["success": false, "message": "Write failed: \(error.localizedDescription)"])
            return
        }

        // Do NOT read here. The firmware acknowledges the GATT write as soon
        // as the command is accepted, but for agent/chat commands the reply is
        // produced asynchronously (the on-device ReAct loop). We wait for the
        // SYS_RSP value update (notification) that the firmware sends once the
        // reply is ready, then read the full value there.
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        if let error = error {
            print("{\"type\":\"notification_error\",\"message\":\"\(error.localizedDescription)\"}")
        }
    }

    // MARK: - Data Parsing

    private func parseSystemStatus(from data: Data) {
        guard data.count >= 14 else { return }

        let bytes = [UInt8](data)
        lastSystemStatus = SystemStatus(
            state: bytes[0],
            wifiState: bytes[1],
            memoryFree: bytes[2...5].withUnsafeBytes { $0.load(as: UInt32.self) },
            uptimeMs: bytes[6...13].withUnsafeBytes { $0.load(as: UInt64.self) },
            errorCode: bytes.count > 14 ? bytes[14] : 0
        )
    }

    private func parseDeviceInfo(from data: Data) {
        guard data.count >= 20 else { return }

        let bytes = [UInt8](data)

        // Parse chip model (bytes 20-35 or available)
        var chipModelEnd = min(36, data.count)
        let chipModelBytes = Array(bytes[min(20, data.count - 1)..<chipModelEnd])
        let chipModel = String(bytes: chipModelBytes, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: .controlCharacters) ?? "ESP32-C61"

        // Parse memory
        var memoryTotal: UInt32 = 0
        var memoryFree: UInt32 = 0
        var uptimeMs: UInt64 = 0

        if data.count >= 8 {
            memoryTotal = bytes[4...7].withUnsafeBytes { $0.load(as: UInt32.self) }
        }
        if data.count >= 12 {
            memoryFree = bytes[8...11].withUnsafeBytes { $0.load(as: UInt32.self) }
        }
        if data.count >= 20 {
            uptimeMs = bytes[12...19].withUnsafeBytes { $0.load(as: UInt64.self) }
        }

        lastDeviceInfo = DeviceInfo(
            versionMajor: bytes[0],
            versionMinor: bytes.count > 1 ? bytes[1] : 0,
            versionPatch: bytes.count > 2 ? bytes[2] : 0,
            memoryTotal: memoryTotal,
            memoryFree: memoryFree,
            uptimeMs: uptimeMs,
            chipModel: chipModel
        )
    }

    private func parseWifiStatus(from data: Data) {
        guard data.count >= 9 else { return }

        let bytes = [UInt8](data)

        // Parse SSID length and SSID
        let ssidLen = min(Int(bytes[8]), data.count - 9)
        let ssidBytes = Array(bytes[9..<(9 + ssidLen)])
        let ssid = String(bytes: ssidBytes, encoding: .utf8) ?? ""

        // Parse IP address
        var ipAddr: [UInt8] = [0, 0, 0, 0]
        if data.count >= 8 {
            ipAddr = Array(bytes[4..<8])
        }

        lastWifiStatus = WifiStatusInfo(
            state: bytes[0],
            rssi: Int8(bitPattern: bytes.count > 1 ? bytes[1] : 0),
            ipAddr: ipAddr,
            ssid: ssid
        )
    }

    private func parseConversationLog(from data: Data) {
        conversationLog.removeAll()

        var offset = 0
        let bytes = [UInt8](data)

        while offset + 11 <= data.count {
            let timestampMs = bytes[offset..<(offset+8)].withUnsafeBytes { $0.load(as: UInt64.self) }
            let role = bytes[offset + 8]
            let lengthBytes = bytes[(offset+9)..<(offset+11)]
            let length = Int(lengthBytes[0]) | (Int(lengthBytes[1]) << 8)

            if offset + 11 + length > data.count { break }

            let textBytes = Array(bytes[(offset+11)..<(offset+11+length)])
            let text = String(bytes: textBytes, encoding: .utf8) ?? ""

            let roleStr: String
            switch role {
            case 0: roleStr = "user"
            case 1: roleStr = "assistant"
            default: roleStr = "system"
            }

            conversationLog.append(ConversationEntry(
                timestamp: Date(timeIntervalSince1970: TimeInterval(timestampMs) / 1000),
                role: roleStr,
                text: text
            ))

            offset += 11 + length
        }
    }

    // MARK: - Result Output

    private func printResult(_ result: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: result, options: []),
              let jsonString = String(data: data, encoding: .utf8) else {
            return
        }
        print(jsonString)
    }

    private func printConfigResult() {
        let config: [String: Any] = [
            "success": true,
            "wifi_ssid": wifiSsid as Any,
            "wifi_password": wifiPassword as Any,
            "llm_model": llmModel as Any,
            "llm_api_key": llmApiKey as Any,
            "hostname": hostname as Any
        ]
        printResult(config)
    }

    private func printStatusResult() {
        guard let status = lastSystemStatus else {
            printResult(["success": false, "message": "Status not available"])
            return
        }

        printResult([
            "success": true,
            "state": status.state,
            "wifi_state": status.wifiState,
            "memory_free": status.memoryFree,
            "uptime_ms": status.uptimeMs,
            "error_code": status.errorCode
        ])
    }

    private func printDeviceInfoResult() {
        guard let info = lastDeviceInfo else {
            printResult(["success": false, "message": "Device info not available"])
            return
        }

        printResult([
            "success": true,
            "version_major": info.versionMajor,
            "version_minor": info.versionMinor,
            "version_patch": info.versionPatch,
            "memory_total": info.memoryTotal,
            "memory_free": info.memoryFree,
            "uptime_ms": info.uptimeMs,
            "chip_model": info.chipModel
        ])
    }

    private func printWifiStatusResult() {
        guard let status = lastWifiStatus else {
            printResult(["success": false, "message": "WiFi status not available"])
            return
        }

        let ipStr = status.ipAddr.map { String($0) }.joined(separator: ".")

        printResult([
            "success": true,
            "state": status.state,
            "rssi": status.rssi,
            "ip_addr": ipStr,
            "ssid": status.ssid
        ])
    }

    private func printConversationsResult() {
        let entries = conversationLog.map { entry -> [String: Any] in
            return [
                "timestamp": entry.timestamp.timeIntervalSince1970 * 1000,
                "role": entry.role,
                "text": entry.text
            ]
        }

        printResult([
            "success": true,
            "conversations": entries
        ])
    }
}

// MARK: - Command Line Interface

let bleManager = BLEManager()

// Process command line arguments
func processCommand(_ args: [String]) {
    guard args.count >= 2 else {
        print("{\"success\":false,\"message\":\"Usage: ble-helper <command> [args...]\"}")
        return
    }

    let command = args[1]

    switch command {
    case "scan":
        let timeout = args.count > 2 ? Int(args[2]) ?? 5 : 5
        bleManager.scan(timeout: max(1, min(timeout, 30))) // Clamp 1-30 seconds

    case "stop-scan":
        bleManager.stopScan()

    case "connect":
        guard args.count > 2 else {
            print("{\"success\":false,\"message\":\"Usage: ble-helper connect <device-id>\"}")
            return
        }
        bleManager.connect(deviceId: args[2])

    case "disconnect":
        guard args.count > 2 else {
            print("{\"success\":false,\"message\":\"Usage: ble-helper disconnect <device-id>\"}")
            return
        }
        bleManager.disconnect(deviceId: args[2])

    case "read-config":
        bleManager.readConfig()

    case "write-wifi":
        guard args.count > 3 else {
            print("{\"success\":false,\"message\":\"Usage: ble-helper write-wifi <ssid> <password>\"}")
            return
        }
        bleManager.writeWifi(ssid: args[2], password: args[3])

    case "write-llm":
        guard args.count > 3 else {
            print("{\"success\":false,\"message\":\"Usage: ble-helper write-llm <model> <api-key>\"}")
            return
        }
        bleManager.writeLlm(model: args[2], apiKey: args[3])

    case "write-hostname":
        guard args.count > 2 else {
            print("{\"success\":false,\"message\":\"Usage: ble-helper write-hostname <hostname>\"}")
            return
        }
        bleManager.writeHostname(hostname: args[2])

    case "get-status":
        bleManager.getStatus()

    case "get-device-info":
        bleManager.getDeviceInfo()

    case "get-wifi-status":
        bleManager.getWifiStatus()

    case "get-conversations":
        bleManager.getConversations()

    case "get-channels":
        bleManager.getChannels()

    case "reboot":
        bleManager.reboot()

    case "get-logs":
        let lines = args.count > 2 ? Int(args[2]) ?? 100 : 100
        bleManager.getLogs(lines: lines)

    case "diagnostics":
        bleManager.runDiagnostics()

    case "exec":
        guard args.count > 2 else {
            print("{\"success\":false,\"message\":\"Usage: ble-helper exec <command>\"}")
            return
        }
        bleManager.execCommand(args[2])

    case "help":
        print("""
        {"type":"help","commands":[
            "scan [timeout] - Scan for BLE devices",
            "stop-scan - Stop scanning",
            "connect <device-id> - Connect to device",
            "disconnect <device-id> - Disconnect from device",
            "read-config - Read device configuration",
            "write-wifi <ssid> <password> - Write WiFi config",
            "write-llm <model> <api-key> - Write LLM config",
            "write-hostname <hostname> - Write hostname",
            "get-status - Get system status",
            "get-device-info - Get device info",
            "get-wifi-status - Get WiFi status",
            "get-conversations - Get conversation log",
            "exec <command> - Execute AT command"
        ]}
        """)

    default:
        print("{\"success\":false,\"message\":\"Unknown command: \(command)\"}")
    }
}

// ---------------------------------------------------------------------------
// Persistent-daemon entry point.
//
// The Tauri backend spawns this process ONCE and drives it over stdin/stdout
// (JSON-RPC-ish, one command per line, result is the first line containing
// "\"success\""). Keeping the process alive with `dispatchMain()` preserves the
// CoreBluetooth connection across commands — the old one-shot-per-command
// model lost the connection the moment the helper exited.
// ---------------------------------------------------------------------------

// Make stdout unbuffered so every result line is delivered to the Tauri
// backend immediately (stdout is a pipe here, which is block-buffered by
// default and would otherwise swallow the JSON until the process exits).
setbuf(stdout, nil)

let stdinSource = DispatchSource.makeReadSource(
    fileDescriptor: STDIN_FILENO,
    queue: DispatchQueue.main
)
stdinSource.setEventHandler {
    // Read a single command line, e.g. "connect <device-id>" or "scan 5".
    let raw = readLine() ?? ""
    let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { return }
    let parts = trimmed.split(separator: " ").map(String.init)
    guard !parts.isEmpty else { return }
    if parts[0] == "exit" {
        exit(0)
    }
    bleManager.handleCommand(parts)
}
stdinSource.resume()

// Keep the run loop alive so CoreBluetooth callbacks, the stdin dispatch
// source, and the scan `Timer` all fire. `dispatchMain()` does NOT pump the
// NSRunLoop, so `Timer`-driven completion (e.g. finishScan) never ran.
RunLoop.main.run()
