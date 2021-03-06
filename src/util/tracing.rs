// TRACING - Implements a scene, camera, ray, and other tracing utilities

#![allow(dead_code)]

////////////////////////////////////////////////////////
/////   INCLUDES
////////////////////////////////////////////////////////
use image::*;
use cgmath::*;
use rand::Rng;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use rayon::prelude::*;
use std::ops::Neg;

use super::geometry::*;
use super::materials::*;

////////////////////////////////////////////////////////
/////   CONSTANTS, TYPEDEFS, ENUMS
////////////////////////////////////////////////////////
pub type Vec3 = Vector3<f32>;
pub type Vec2 = Vector2<f32>;
pub type Color = Vec3;

#[derive(Debug, Clone, Copy)]
pub enum CameraProjectionMode {
    Orthographic,
    Perspective,
}
#[derive(Debug, Clone, Copy)]
pub enum ShadingMode {
    Phong,
    PathTrace,
}

////////////////////////////////////////////////////////
/////   TRAITS
////////////////////////////////////////////////////////

// Define trait for anything that can intersect a ray
pub trait Intersectable {
    // tests for intersection with a given ray and returns hit info
    fn intersect_ray(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<RayHit>;
    // returns the axis-aligned bounding box of the intersectable, if there is one
    fn bounding_box(&self) -> Option<AABB>; // Option because not all primitives have bounding boxes (e.g. plane)
}


////////////////////////////////////////////////////////
/////   UTILITY FUNCTIONS
////////////////////////////////////////////////////////
// reflect a vector about a normal
pub fn reflect(v: &Vec3, n: &Vec3) -> Vec3 {
    v - 2.0*v.dot(*n)*n
}
// Approximates the fresnel reflection-transmission coefficient using Schlick's approximation (https://en.wikipedia.org/wiki/Schlick%27s_approximation)
pub fn fresnel(v: &Vec3, n: &Vec3, ir: f32) -> f32 {
    // (first index of refraction is assumed to be air (1.0). the equation is symmetric so it doesn't matter which medium is first)
    let r0 = ((ir-1.0)/(ir+1.0)).powi(2);
    r0 + (1.0-r0)*(1.0-v.dot(*n).abs()).powi(5)
}
// Refract function from raytracing in one weekend:
pub fn refract(v: &Vec3, n: &Vec3, eta: f32) -> Vec3 {
    let cos_theta = f32::min((v.neg()).dot(*n), 1.0);
    let r_out_perp =  eta * (v + cos_theta*n);
    let r_out_parallel = -f32::sqrt((1.0 - r_out_perp.magnitude2()).abs()) * n;
    return r_out_perp + r_out_parallel;
}
// random vector in a unit sphere (rejection method)
pub fn rand_sphere_vec() -> Vec3 {
    let mut rng = rand::thread_rng();
    loop {
        let dir = Vec3 { x: rng.gen_range(-1.0..1.0), y: rng.gen_range(-1.0..1.0), z: rng.gen_range(-1.0..1.0) };
        if dir.magnitude2() <= 1.0 {
            return dir;
        }
    }
}
// random vector in a unit disk in xy plane (rejection method)
pub fn rand_disk_vec() -> Vec3 {
    let mut rng = rand::thread_rng();
    loop {
        let dir = Vec3 { x: rng.gen_range(-1.0..1.0), y: rng.gen_range(-1.0..1.0), z: 0.0 };
        if dir.magnitude2() <= 1.0 {
            return dir;
        }
    }
}
// clamps a vector
pub fn clampvec(v: Vec3, min: f32, max: f32) -> Vec3 {
    vec3(v.x.clamp(min, max), v.y.clamp(min, max), v.z.clamp(min, max))
}
// linear interpolation for vectors
pub fn lerpvec(a: Vec3, b: Vec3, k: f32) -> Vec3 {
    (1.0-k)*a+k*b
}

////////////////////////////////////////////////////////
/////   CLASSES
////////////////////////////////////////////////////////

// RAY / RAYHIT
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
}
#[derive(Clone)]
pub struct RayHit {
    pub distance: f32,  // from origin to hit point
    pub hitpoint: Vec3, // location of intersection
    pub normal: Vec3,   // normal at hit point
    pub material: Arc<dyn Material + Send + Sync>, // material properties at hit point
    pub frontface: bool,            // whether the ray hit the front or back of surface
    pub tex_coords: Option<Vec2>,   // tex coords at hit point
    pub tangent: Option<Vec3>,      // tangent vector at hit point
    pub bitangent: Option<Vec3>,    // bitangent vector at hit point
}
impl RayHit {
    // ray hit constructor
    pub fn new(distance: f32, normal: Vec3, material: Arc<dyn Material + Send + Sync>, ray: &Ray) -> RayHit {
        let frontface = normal.dot(ray.direction) < 0.0;
        RayHit { 
            distance: distance,
            hitpoint: ray.origin+ray.direction*distance,
            normal: if frontface {normal} else {-normal},
            material: material,
            frontface: frontface,
            tex_coords: None,
            tangent: None,
            bitangent: None,
        }
    }
}

// CAMERA
#[derive(Debug, Clone)]
pub struct Camera {
    // camera model based on 419 lectures
    pub eyepoint: Vec3, // 3d location of camera
    pub view_dir: Vec3, // direction from eyepoint through center of image plane
    pub up: Vec3,       // camera up vector
    pub projection_mode: CameraProjectionMode,
    pub shading_mode: ShadingMode,
    pub path_depth: u32,        // recursion depth for rendering equation
    pub path_samples: u32,      // number of sample rays to generate per recurive step (anything above 1 is unnecessary)
    pub screen_width: u32,      // in pixels
    pub screen_height: u32,     // ""
    pub focal_length: f32,      // distance from eyepoint to image plane
    pub focus_dist: f32,        // distance from eyepoint to plane where everything is in focus
    pub lens_radius: f32,       // radius of approximated thin lens
    pub aa_sample_count: u32,   // number of samples per pixel (should be perfect square)
    pub max_trace_dist: f32,    // maximum distance from ray origin to consider intersections
    pub gamma: f32,             // color gamma correction
}
impl Camera {
    // generate camera rays given pixel coordinates and sample count
    // currently uses multi-jittered sampling
    pub fn generate_rays(&self, screen_x: u32, screen_y: u32) -> Vec<Ray> {
        let pixel_size = 1.0 / self.screen_height as f32;
        let mut rays = Vec::new();
        let n = self.aa_sample_count as f32;
        let rootn = n.sqrt();
        let mut rng = rand::thread_rng();
        for i in 0..self.aa_sample_count {
            // compute multi-jittered pixel offset
            let rand_x = rng.gen_range(0..self.aa_sample_count) as f32;
            let rand_y = rng.gen_range(0..self.aa_sample_count) as f32;
            let subpixel_x = (i / rootn as u32) as f32;
            let subpixel_y = (i % rootn as u32) as f32;
            let subpixel_offset = vec2(
                (subpixel_x - 0.5*rootn)*pixel_size/rootn + (rand_x - 0.5*n)*pixel_size/n,
                (subpixel_y - 0.5*rootn)*pixel_size/rootn + (rand_y - 0.5*n)*pixel_size/n,
             );
            
            // compute pixel center and offset by jitter
            let cam_space_pixel_center = vec3(
                pixel_size*(screen_x as f32 - 0.5*(self.screen_width as f32) + 0.5) + subpixel_offset.x,
                pixel_size*(0.5 + 0.5*(self.screen_height as f32) - screen_y as f32) + subpixel_offset.y,
                -self.focal_length
            );
            // cast ray from random location in disk to point on focus plane
            let focus_plane_pixel_center = cam_space_pixel_center.normalize()*self.focus_dist;
            let lens_origin = self.lens_radius*rand_disk_vec();

            // find rotation from camera to world space:
            let rotation = Matrix3::from_cols(
                self.view_dir.cross(self.up).normalize(),
                self.up,
                -self.view_dir
            );
           
            // create ray with direction still in camera space
            let mut ray = Ray {
                origin: match self.projection_mode {
                    CameraProjectionMode::Orthographic => vec3(cam_space_pixel_center.x, cam_space_pixel_center.y, 0.0 ),
                    CameraProjectionMode::Perspective => self.eyepoint + rotation*lens_origin,
                },
                direction: match self.projection_mode {
                    CameraProjectionMode::Orthographic => self.view_dir,
                    CameraProjectionMode::Perspective => (focus_plane_pixel_center - lens_origin).normalize()
                },
            };
            ray.direction = rotation * ray.direction;

            rays.push(ray);
        }
        return rays;
    }
}

// SCENE
pub struct Scene {
    pub camera: Camera,
    pub objects: Arc<Vec<Arc<dyn Intersectable + Send + Sync>>>,
    pub point_light_pos: Vec3,  // point light only used for phong shading, which was just for debuging
    pub ambient: Vec3,          // ambient light used for phong shading (and possibly when pathtracing stops recursing)
}
impl Scene {
    // render scene to image
    pub fn render_to_image(&self) -> RgbImage {
        println!("Rendering...");
        let progress_bar = ProgressBar::new((self.camera.screen_width*self.camera.screen_height) as u64);
        progress_bar.set_style(ProgressStyle::default_bar().template("[{elapsed_precise}, {eta_precise}] {wide_bar:.green/blue} {pos:>7}/{len:7}").progress_chars("##-"));
        // create image and thread channel
        let mut img = RgbImage::new(self.camera.screen_width, self.camera.screen_height);
        // iterate through pixels...
        img.as_parallel_slice_mut().into_par_iter().chunks(self.camera.screen_width as usize * 3).enumerate().for_each(|(y, mut data)| {
            for x in 0..self.camera.screen_width as usize {
                // get rays, trace rays, and take average of outputs for AA
                let cam_rays = self.camera.generate_rays(x as u32, y as u32);
                let mut final_color = Vec3::zero();
                for sample_idx in 0..cam_rays.len() {
                    if matches!(self.camera.shading_mode, ShadingMode::Phong) {
                        final_color += self.phong_shade_ray(&cam_rays[sample_idx]);
                    }
                    else {
                        final_color += self.shade_ray(&cam_rays[sample_idx], 0);
                    }
                }
                final_color = final_color / cam_rays.len() as f32;
                
                // saturate colors towards white if they are excessively bright
                let tmp = final_color.clone();
                for i in 0..3 {
                    let d = tmp[i] - 1.0;
                    if d > 0.0 {
                        final_color[(i+1)%3] += d;
                        final_color[(i+2)%3] += d;
                    }
                }

                // write to image
                *(data[3*x])   = (f32::powf(final_color.x.clamp(0.0,1.0), 1.0/self.camera.gamma) * 255.9999) as u8;
                *(data[3*x+1]) = (f32::powf(final_color.y.clamp(0.0,1.0), 1.0/self.camera.gamma) * 255.9999) as u8;
                *(data[3*x+2]) = (f32::powf(final_color.z.clamp(0.0,1.0), 1.0/self.camera.gamma) * 255.9999) as u8;
                progress_bar.inc(1);
            }
        });
        progress_bar.finish();
        println!("Done.");
        return img;
    }
    
    // defines background color in a given direction
    fn background_color(_v: &Vec3) -> Color {
        // used to use blue gradient from raytracing in one weekend
        // let u = v.normalize();
        // let t = 0.5*(u.y+1.0);
        // (1.0-t)*vec3(1.0, 1.0, 1.0) + t*vec3(0.5, 0.7, 1.0)
        
        // now just use black void:
        Vec3::zero()
    }
    
    // computes phong shading for a given rayhit. usually just used for debugging
    fn phong_shade_ray(&self, ray: &Ray) -> Color {
        // get hit
        match self.intersect_ray(ray, 0.0, self.camera.max_trace_dist) {
            None => Scene::background_color(&ray.direction),
            Some(hit) => {
                // standard phong shading
                let to_light = (self.point_light_pos - hit.hitpoint).normalize();
                let to_camera = (self.camera.eyepoint - hit.hitpoint).normalize();
                let reflected = -to_light + 2.0*dot(to_light, hit.normal)*hit.normal;
                let diffuse_weight = (dot(hit.normal, to_light)).clamp(0.0, 1.0);
                let specular_weight = dot(to_camera, reflected).clamp(0.0, 1.0).powf(40.0);
                // cast shadow ray
                let shadow_ray = Ray { origin: hit.hitpoint + 0.01*hit.normal, direction: to_light };
                let shadow_weight = match self.intersect_ray(&shadow_ray, 0.0, (self.point_light_pos - hit.hitpoint).magnitude()) {
                    None => 1.0,
                    Some(hit) => if hit.distance*hit.distance > (self.point_light_pos - hit.hitpoint).magnitude2() { 1.0 } else { 0.3 }
                };
                shadow_weight * (self.ambient + diffuse_weight*hit.material.scatter(&hit, ray).1 + specular_weight*vec3(0.4, 0.4, 0.4))
            }
        }
    }
    
    // computes shading for a ray hit according to the monte-carlo integrated rendering equation
    fn shade_ray(&self, ray: &Ray, recursion_depth: u32) -> Color {
        if recursion_depth >= self.camera.path_depth { 
            return Scene::background_color(&ray.direction); // approximates the remaining infinite recursion results
        }
        // get hit
        match self.intersect_ray(ray, 0.001, self.camera.max_trace_dist.clone()) {
            None => Scene::background_color(&ray.direction),
            Some(hit) => {
                // accumulate integral
                let mut integral = Color::zero();
                for _i in 0..self.camera.path_samples {
                    // pick new direction, generate ray, and recurse
                    let (new_ray, brdf_term, pdf) = hit.material.scatter(&hit, ray);
                    let dot_term = if hit.normal.magnitude2() > 0.0 {new_ray.direction.dot(hit.normal).abs().clamp(0.0,1.0)} else {1.0};
                    let incoming_light = self.shade_ray(&new_ray, recursion_depth+1);
                    // accumulate into integral
                    integral += (dot_term*(brdf_term.mul_element_wise(incoming_light))) / pdf;
                }
                integral /= self.camera.path_samples as f32; 
        
                // total light = integrated + emitted light
                hit.material.emission() + integral
            }
        }        
    }
}
impl Intersectable for Scene {
    fn intersect_ray(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<RayHit> {
        // iterate over all objects in the list and return the closest intersection
        let mut best_hit = None;
        for object in self.objects.iter() {
            if let Some(hit) = object.intersect_ray(ray, t_min, t_max) {
                best_hit = match best_hit {
                    None => Some(hit),
                    Some(current_best) => {
                        if hit.distance < current_best.distance {
                            Some(hit)
                        }
                        else {
                            Some(current_best)
                        }
                    }
                }
            }
        }
        return best_hit;
    }
    fn bounding_box(&self) -> Option<AABB> {
        None    // we don't really need a bounding box for the entire scene right now
    }
}


// runs ray tracer
pub fn run() {
    // initialize scene
    let scene = Scene {
        camera: Camera {
            eyepoint: vec3(0.0, 2.0, 5.5),
            view_dir: -Vec3::unit_z(),
            up: Vec3::unit_y(),
            focal_length: 0.6,  // distance from eyepoint to image plane
            focus_dist: 5.0,    // distance from eyepoint to focus plane
            lens_radius: 0.0,   // radius of thin-lens approximation
            projection_mode: CameraProjectionMode::Perspective,
            shading_mode: ShadingMode::PathTrace,
            screen_width: 100,
            screen_height: 100,
            aa_sample_count: 100,
            path_depth: 10,     // path-tracing recursion depth
            path_samples: 1,    // sub-rays cast per recursion (slow if more than 1)
            max_trace_dist: 100.0,
            gamma: 2.0,
        },
        objects: Arc::new(vec![
            Arc::new(StaticMesh::load_from_file(
                "./obj/drone.obj",
                Some("./texture/Drone_Albedo.tga"),
                Some("./texture/Drone_Emission.tga"),
                Some("./texture/Drone_Metallic.tga"),
                Some("./texture/Drone_Roughness.tga"),
                Some("./texture/Drone_Normal.tga"),
                None,
                Matrix4::from_translation(vec3(0.0,1.3,1.7))*Matrix4::from_angle_y(Deg(-60.0))*Matrix4::from_angle_x(Deg(180.0))*Matrix4::from_scale(0.0030)
            )), 
            Arc::new(StaticMesh::load_from_file(
                "./obj/cube.obj",
                Some("./texture/green.png"),
                None,
                None,
                None,
                Some("./texture/normal_test.jpg"),
                None,
                Matrix4::from_translation(vec3(-1.7,0.5,2.7))*Matrix4::from_angle_y(Deg(45.0))*Matrix4::from_scale(0.4),
            )),          
            Arc::new(StaticMesh::load_from_file(
                "./obj/sphere.obj",
                Some("./texture/magenta.jpg"),
                None,
                None,
                None,
                Some("./texture/normal_test.png"),
                None,
                Matrix4::from_translation(vec3(1.7,0.5,2.7))*Matrix4::from_angle_y(Deg(45.0))*Matrix4::from_scale(0.6),
            )),     
            
            // DEMO OF PARAMETERIZED MATERIAL
            Arc::new(Sphere {
                center: vec3(-2.6,3.3,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.0, metallic: 0.0})
            }),
            Arc::new(Sphere {
                center: vec3(-1.3,3.3,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.25, metallic: 0.0})
            }),
            Arc::new(Sphere {
                center: vec3(0.0,3.3,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.5, metallic: 0.0})
            }),
            Arc::new(Sphere {
                center: vec3(1.3,3.3,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.75, metallic: 0.0})
            }),
            Arc::new(Sphere {
                center: vec3(2.6,3.3,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 1.0, metallic: 0.0})
            }),
            
            Arc::new(Sphere {
                center: vec3(-2.6,4.4,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.0, metallic: 0.5})
            }),
            Arc::new(Sphere {
                center: vec3(-1.3,4.4,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.25, metallic: 0.5})
            }),
            Arc::new(Sphere {
                center: vec3(0.0,4.4,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.5, metallic: 0.5})
            }),
            Arc::new(Sphere {
                center: vec3(1.3,4.4,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.75, metallic: 0.5})
            }),
            Arc::new(Sphere {
                center: vec3(2.6,4.4,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 1.0, metallic: 0.5})
            }),

            Arc::new(Sphere {
                center: vec3(-2.6,5.5,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.0, metallic: 1.0})
            }),
            Arc::new(Sphere {
                center: vec3(-1.3,5.5,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.25, metallic: 1.0})
            }),
            Arc::new(Sphere {
                center: vec3(0.0,5.5,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.5, metallic: 1.0})
            }),
            Arc::new(Sphere {
                center: vec3(1.3,5.5,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 0.75, metallic: 1.0})
            }),
            Arc::new(Sphere {
                center: vec3(2.6,5.5,0.0),
                radius: 0.5,
                material: Arc::new(ParameterizedMaterial{albedo: vec3(0.01,0.02,0.5), emission: Vec3::zero(), roughness: 1.0, metallic: 1.0})
            }),
            
            
            
            // VARIOUS OTHER OBJECTS

            Arc::new(Sphere {
                center: vec3(-2.3,2.0,2.0),
                radius: 0.4,
                material: Arc::new(Dielectric { idx_of_refraction: 2.5 })
            }),
            Arc::new(Sphere {
                center: vec3(2.3,2.0,2.0),
                radius: 0.4,
                material: Arc::new(Lambertian { albedo: vec3(0.3,0.3,0.3), emission: vec3(0.0,1.0,1.0),}),
            }),
            Arc::new(ConvexVolume {
                boundary: Arc::new(Sphere {
                    center: vec3(-3.0,1.0,1.0),
                    radius: 1.0,
                    material: Arc::new(Dielectric { idx_of_refraction: 1.5 }) /* arbitrary */,
                }),
                phase_function: Arc::new(Isotropic { albedo: vec3(1.0,1.0,1.0), emission: Vec3::zero() }),
                density: 0.6,
            }),
            Arc::new(ConvexVolume {
                boundary: Arc::new(Sphere {
                    center: vec3(3.0,1.0,1.0),
                    radius: 1.0,
                    material: Arc::new(Dielectric { idx_of_refraction: 1.5 }) /* arbitrary */,
                }),
                phase_function: Arc::new(Isotropic { albedo: vec3(0.0,0.0,0.0), emission: Vec3::zero() }),
                density: 0.8,
            }),

            // Floor
            Arc::new(Plane {
                point: vec3(0.0, 0.0, 0.0),
                normal: Vec3::unit_y(),
                // material: Arc::new(Lambertian { albedo: vec3(0.33,0.33,0.33), ..Default::default() }),
                material: Arc::new(ParameterizedMaterial { albedo: vec3(0.33,0.33,0.33), emission: Vec3::zero(), metallic: 0.3, roughness: 0.7 }),
            }),  
            
            // LIGHT
            Arc::new(Triangle {
                a: vec3(-2.5, 7.5, -0.5),
                b: vec3(2.5, 7.5,  -0.5),
                c: vec3(2.5, 7.5, 3.5),
                material: Arc::new(Lambertian { albedo: vec3(0.0,0.6,0.0), emission: vec3(7.0,7.0,7.0), ..Default::default() }),
            }),
            Arc::new(Triangle {
                a: vec3(-2.5, 7.5, -0.5),
                b: vec3(-2.5, 7.5,  3.5),
                c: vec3(2.5, 7.5, 3.5),
                material: Arc::new(Lambertian { albedo: vec3(0.0,0.6,0.0), emission: vec3(7.0,7.0,7.0), ..Default::default() }),
            }),

        ]),
        point_light_pos: vec3(0.0,1.0,5.0), // for phong shading only
        ambient: vec3(0.1,0.1,0.1), // for phong shading only
    };

    // render and write output
    scene.render_to_image().save_with_format("render.png", ImageFormat::Png).unwrap();

}