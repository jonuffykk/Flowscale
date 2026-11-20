use ash::vk;
fn main() {
    unsafe {
        let entry = ash::Entry::load().expect("vulkan-1.dll");
        let layers = entry.enumerate_instance_layer_properties().unwrap();
        for l in &layers { println!("layer: {}", l.layer_name_as_c_str().unwrap().to_string_lossy()); }
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
        let exts = [ash::khr::surface::NAME.as_ptr(), ash::khr::win32_surface::NAME.as_ptr()];
        let inst = entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(&exts), None).expect("vkCreateInstance through layer");
        let pd = inst.enumerate_physical_devices().unwrap()[0];
        let props = inst.get_physical_device_properties(pd);
        println!("gpu: {}", props.device_name_as_c_str().unwrap().to_string_lossy());
        let prio = [1.0f32];
        let qci = [vk::DeviceQueueCreateInfo::default().queue_family_index(0).queue_priorities(&prio)];
        let dexts = [ash::khr::swapchain::NAME.as_ptr()];
        let dev = inst.create_device(pd, &vk::DeviceCreateInfo::default().queue_create_infos(&qci).enabled_extension_names(&dexts), None).expect("vkCreateDevice through layer");
        let gdpa = inst.fp_v1_0().get_device_proc_addr;
        let p = gdpa(dev.handle(), c"vkQueuePresentKHR".as_ptr());
        println!("vkQueuePresentKHR resolved: {}", p.is_some());
        let q = dev.get_device_queue(0, 0);
        let _ = q;
        dev.destroy_device(None);
        inst.destroy_instance(None);
        println!("ok");
    }
}
