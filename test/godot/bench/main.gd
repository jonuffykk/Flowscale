extends Node3D
var cam: Camera3D
var hud: Label
var t := 0.0
var boxes: Array[MeshInstance3D] = []

func _ready() -> void:
	DisplayServer.window_set_title("FlowScale Bench (%s)" % RenderingServer.get_current_rendering_driver_name())
	var env := WorldEnvironment.new(); env.environment = Environment.new()
	env.environment.background_mode = Environment.BG_COLOR; env.environment.background_color = Color(0.08, 0.09, 0.11)
	env.environment.ambient_light_color = Color(0.4, 0.42, 0.5); env.environment.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	add_child(env)
	var sun := DirectionalLight3D.new(); sun.rotation_degrees = Vector3(-50, 30, 0); sun.shadow_enabled = true; add_child(sun)
	cam = Camera3D.new(); add_child(cam)
	var mesh := BoxMesh.new()
	for x in range(20):
		for z in range(20):
			var m := MeshInstance3D.new(); m.mesh = mesh
			var mat := StandardMaterial3D.new(); mat.albedo_color = Color.from_hsv(fmod((x * 20 + z) * 0.037, 1.0), 0.6, 0.9)
			m.material_override = mat
			m.position = Vector3((x - 10) * 2.2, 0, (z - 10) * 2.2)
			add_child(m); boxes.append(m)
	var floor := MeshInstance3D.new(); var pm := PlaneMesh.new(); pm.size = Vector2(80, 80); floor.mesh = pm; floor.position.y = -1; add_child(floor)
	var layer := CanvasLayer.new(); add_child(layer)
	hud = Label.new(); hud.position = Vector2(16, 12); hud.add_theme_font_size_override("font_size", 22); layer.add_child(hud)

func _process(delta: float) -> void:
	t += delta
	cam.position = Vector3(sin(t * 0.6) * 18.0, 9.0 + sin(t * 0.9) * 2.0, cos(t * 0.6) * 18.0)
	cam.look_at(Vector3.ZERO)
	for i in boxes.size():
		var b := boxes[i]
		b.position.y = sin(t * 2.0 + i * 0.17) * 0.8
		b.rotation.y = t + i * 0.05
	hud.text = "HUD  %d fps" % Engine.get_frames_per_second()
