# changed id between passes

2026-06-11T21:00:58.977854Z  INFO delog: DeLOG starting version="0.1.0"
2026-06-11T21:00:59.025441Z  INFO wgpu_hal::gles::egl: EGL says it can present to the window but not natively
2026-06-11T21:00:59.033332Z  INFO wgpu_hal::vulkan::adapter: Found 6 cooperative matrix configurations supported by wgpu
2026-06-11T21:00:59.072238Z  INFO wgpu_hal::vulkan::adapter: Found 6 cooperative matrix configurations supported by wgpu
2026-06-11T21:00:59.073535Z  INFO egui_wgpu: There are 2 available wgpu adapters: {backend: Vulkan, device_type: DiscreteGpu, name: "NVIDIA GeForce RTX 4080", driver: "NVIDIA", driver_info: "595.71.05", vendor: NVIDIA (0x10DE), device: 0x2704, pci_bus_id: "0000:01:00.0", subgroup_size: 32..=32, transient_saves_memory: false}, {backend: Gl, device_type: Other, name: "NVIDIA GeForce RTX 4080/PCIe/SSE2", driver_info: "3.3.0 NVIDIA 595.71.05", vendor: NVIDIA (0x10DE), subgroup_size: 4..=128, transient_saves_memory: false}
2026-06-11T21:01:08.618541Z  WARN egui::context: Widget rect [[8.0 24.0] - [45.9 45.0]] changed id between passes: prev ids: ["88B1"], new ids: ["0859"]
2026-06-11T21:01:08.618571Z  WARN egui::context: Widget rect [[8.0 48.0] - [272.0 54.0]] changed id between passes: prev ids: ["21B0"], new ids: ["DB4A"]
2026-06-11T21:01:08.844526Z  WARN egui::context: Widget rect [[8.0 24.0] - [45.9 45.0]] changed id between passes: prev ids: ["0859"], new ids: ["78FE"]
2026-06-11T21:01:08.844560Z  WARN egui::context: Widget rect [[8.0 48.0] - [365.8 54.0]] changed id between passes: prev ids: ["DB4A"], new ids: ["7940"]
2026-06-11T21:01:08.844573Z  WARN egui::context: Widget rect [[8.0 57.0] - [365.8 76.0]] changed id between passes: prev ids: ["1768", "AD0C"], new ids: ["8930", "37B5"]
2026-06-11T21:01:08.844586Z  WARN egui::context: Widget rect [[8.0 79.0] - [365.8 1231.0]] changed id between passes: prev ids: ["B54F", "9341", "5A13", "FA58"], new ids: ["8BA3", "54EE", "8FB4", "A558"]
2026-06-11T21:01:08.844598Z  WARN egui::context: Widget rect [[8.0 100.0] - [365.8 1231.0]] changed id between passes: prev ids: ["6924"], new ids: ["C852"]
2026-06-11T21:01:08.844611Z  WARN egui::context: Widget rect [[26.0 100.0] - [365.8 118.0]] changed id between passes: prev ids: ["969E"], new ids: ["C530"]
2026-06-11T21:01:08.844622Z  WARN egui::context: Widget rect [[26.0 100.0] - [365.8 1231.0]] changed id between passes: prev ids: ["95C8", "0A32"], new ids: ["BA3E", "AAD9"]
2026-06-11T21:01:08.844633Z  WARN egui::context: Widget rect [[26.0 101.5] - [238.0 116.5]] changed id between passes: prev ids: ["53F7"], new ids: ["AD89"]
2026-06-11T21:01:08.844668Z  WARN egui::context: Widget rect [[246.0 101.5] - [281.1 116.5]] changed id between passes: prev ids: ["0612"], new ids: ["9F0E"]
2026-06-11T21:01:08.844678Z  WARN egui::context: Widget rect [[289.1 100.0] - [338.1 118.0]] changed id between passes: prev ids: ["A439"], new ids: ["8580"]
2026-06-11T21:01:08.844690Z  WARN egui::context: Widget rect [[346.1 101.5] - [365.8 116.5]] changed id between passes: prev ids: ["F8B7"], new ids: ["2B00"]

# App Freezes

2026-06-11T21:36:00.958311Z  INFO delog: DeLOG starting version="0.1.0"
2026-06-11T21:36:01.012921Z  INFO wgpu_hal::gles::egl: EGL says it can present to the window but not natively
2026-06-11T21:36:01.022382Z  INFO wgpu_hal::vulkan::adapter: Found 6 cooperative matrix configurations supported by wgpu
2026-06-11T21:36:01.062959Z  INFO wgpu_hal::vulkan::adapter: Found 6 cooperative matrix configurations supported by wgpu
2026-06-11T21:36:01.064420Z  INFO egui_wgpu: There are 2 available wgpu adapters: {backend: Vulkan, device_type: DiscreteGpu, name: "NVIDIA GeForce RTX 4080", driver: "NVIDIA", driver_info: "595.71.05", vendor: NVIDIA (0x10DE), device: 0x2704, pci_bus_id: "0000:01:00.0", subgroup_size: 32..=32, transient_saves_memory: false}, {backend: Gl, device_type: Other, name: "NVIDIA GeForce RTX 4080/PCIe/SSE2", driver_info: "3.3.0 NVIDIA 595.71.05", vendor: NVIDIA (0x10DE), subgroup_size: 4..=128, transient_saves_memory: false}
2026-06-11T21:36:05.859405Z  WARN egui::context: Widget rect [[8.0 24.0] - [45.9 45.0]] changed id between passes: prev ids: ["88B1"], new ids: ["78FE"]
2026-06-11T21:36:05.859437Z  WARN egui::context: Widget rect [[8.0 48.0] - [272.0 54.0]] changed id between passes: prev ids: ["21B0"], new ids: ["7940"]
2026-06-11T21:36:32.918954Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:33.944005Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:34.967119Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:35.991015Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:37.015016Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:38.038959Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:39.062976Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:40.086978Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:41.111979Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:42.135010Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:43.159974Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:44.182998Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:45.208032Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:46.231151Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:47.255155Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:48.279056Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:49.302969Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:50.327037Z  WARN egui_wgpu: Dropped frame with error: Timeout
2026-06-11T21:36:51.350999Z  WARN egui_wgpu: Dropped frame with error: Timeout
