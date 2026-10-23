use std::collections::BTreeMap;
#[derive(Debug, Copy, Clone)]
pub struct Coord {
    pub x: f64,
    pub y: f64,
}

impl Coord {
    fn to_geomorph_coord(self) -> geomorph::Coord {
        geomorph::Coord {
            lat: self.y,
            lon: self.x,
        }
    }

    fn to_wgs84(self, crs: &UtmCrs) -> Self {
        let (lon, lat) = transverse_mercator::inverse(
            self.x - transverse_mercator::FALSE_EASTING,
            self.y - crs.false_northing(),
            crs.central_meridian(),
        );
        Self { x: lon, y: lat }
    }

    fn conv_from_wgs84(self, crs: &UtmCrs) -> Self {
        // Signed from the equator, so a position in the other hemisphere
        // than the zone's still has a northing in it, as PROJ gives.
        let (x, y) = transverse_mercator::forward(self.x, self.y, crs.central_meridian());
        Self {
            x: transverse_mercator::FALSE_EASTING + x,
            y: crs.false_northing() + y,
        }
    }
}

/// The UTM projection on WGS84, both ways, to PROJ's accuracy (#480).
///
/// Krüger's series to sixth order in the third flattening, as given by
/// Karney (2011), "Transverse Mercator with an accuracy of a few
/// nanometers", J. Geodesy 85, 475-485, and as PROJ implements it (`tmerc`,
/// its default algorithm). It is accurate to well under a millimetre within
/// 3900 km of the central meridian, which covers a whole zone and any
/// position a survey forced into a neighbouring one.
///
/// Before #480 the inverse came from the `geomorph` crate, whose latitudes
/// were off by an amount that grows with latitude (4.4 m at 78 °N), and the
/// forward from the `utm` crate, whose own inverse loses metres away from
/// the central meridian.
mod transverse_mercator {
    /// WGS84 semi-major axis (m) and flattening.
    const A: f64 = 6_378_137.0;
    const F: f64 = 1.0 / 298.257_223_563;
    /// UTM's scale on the central meridian.
    const K0: f64 = 0.9996;
    pub const FALSE_EASTING: f64 = 500_000.0;

    /// The third flattening, the first eccentricity, and the rectifying
    /// radius times `K0`.
    fn constants() -> (f64, f64, f64) {
        let n = F / (2.0 - F);
        let e = (F * (2.0 - F)).sqrt();
        let n2 = n * n;
        let rectifying = A / (1.0 + n) * (1.0 + n2 / 4.0 + n2 * n2 / 64.0 + n2 * n2 * n2 / 256.0);
        (n, e, K0 * rectifying)
    }

    fn alpha(n: f64) -> [f64; 6] {
        let (n2, n3, n4, n5, n6) = (n.powi(2), n.powi(3), n.powi(4), n.powi(5), n.powi(6));
        [
            n / 2.0 - 2.0 * n2 / 3.0 + 5.0 * n3 / 16.0 + 41.0 * n4 / 180.0 - 127.0 * n5 / 288.0
                + 7891.0 * n6 / 37800.0,
            13.0 * n2 / 48.0 - 3.0 * n3 / 5.0 + 557.0 * n4 / 1440.0 + 281.0 * n5 / 630.0
                - 1983433.0 * n6 / 1935360.0,
            61.0 * n3 / 240.0 - 103.0 * n4 / 140.0
                + 15061.0 * n5 / 26880.0
                + 167603.0 * n6 / 181440.0,
            49561.0 * n4 / 161280.0 - 179.0 * n5 / 168.0 + 6601661.0 * n6 / 7257600.0,
            34729.0 * n5 / 80640.0 - 3418889.0 * n6 / 1995840.0,
            212378941.0 * n6 / 319334400.0,
        ]
    }

    fn beta(n: f64) -> [f64; 6] {
        let (n2, n3, n4, n5, n6) = (n.powi(2), n.powi(3), n.powi(4), n.powi(5), n.powi(6));
        [
            n / 2.0 - 2.0 * n2 / 3.0 + 37.0 * n3 / 96.0 - n4 / 360.0 - 81.0 * n5 / 512.0
                + 96199.0 * n6 / 604800.0,
            n2 / 48.0 + n3 / 15.0 - 437.0 * n4 / 1440.0 + 46.0 * n5 / 105.0
                - 1118711.0 * n6 / 3870720.0,
            17.0 * n3 / 480.0 - 37.0 * n4 / 840.0 - 209.0 * n5 / 4480.0 + 5569.0 * n6 / 90720.0,
            4397.0 * n4 / 161280.0 - 11.0 * n5 / 504.0 - 830251.0 * n6 / 7257600.0,
            4583.0 * n5 / 161280.0 - 108847.0 * n6 / 3991680.0,
            20648693.0 * n6 / 638668800.0,
        ]
    }

    /// The conformal latitude's tangent from the geodetic latitude's.
    fn conformal_tan(tau: f64, e: f64) -> f64 {
        let sigma = (e * (e * tau / (1.0 + tau * tau).sqrt()).atanh()).sinh();
        tau * (1.0 + sigma * sigma).sqrt() - sigma * (1.0 + tau * tau).sqrt()
    }

    /// WGS84 longitude and latitude (degrees) to x and y (m) from the
    /// central meridian `lon0` and the equator, before false easting and
    /// northing.
    pub fn forward(lon: f64, lat: f64, lon0: f64) -> (f64, f64) {
        let (n, e, scaled) = constants();
        let lambda = (lon - lon0).to_radians();
        let tau_prime = conformal_tan(lat.to_radians().tan(), e);
        let xi_prime = tau_prime.atan2(lambda.cos());
        let eta_prime =
            (lambda.sin() / (tau_prime * tau_prime + lambda.cos().powi(2)).sqrt()).asinh();
        let (mut xi, mut eta) = (xi_prime, eta_prime);
        for (j, a) in alpha(n).iter().enumerate() {
            let k = 2.0 * (j + 1) as f64;
            xi += a * (k * xi_prime).sin() * (k * eta_prime).cosh();
            eta += a * (k * xi_prime).cos() * (k * eta_prime).sinh();
        }
        (scaled * eta, scaled * xi)
    }

    /// [`forward`]'s inverse.
    pub fn inverse(x: f64, y: f64, lon0: f64) -> (f64, f64) {
        let (n, e, scaled) = constants();
        let (xi, eta) = (y / scaled, x / scaled);
        let (mut xi_prime, mut eta_prime) = (xi, eta);
        for (j, b) in beta(n).iter().enumerate() {
            let k = 2.0 * (j + 1) as f64;
            xi_prime -= b * (k * xi).sin() * (k * eta).cosh();
            eta_prime -= b * (k * xi).cos() * (k * eta).sinh();
        }
        let tau_prime = xi_prime.sin() / (eta_prime.sinh().powi(2) + xi_prime.cos().powi(2)).sqrt();
        // Newton's method on the conformal latitude, which converges in a
        // few steps everywhere (Karney 2011, eq. 19-21).
        let e2 = e * e;
        let mut tau = tau_prime;
        for _ in 0..10 {
            let tau_i_prime = conformal_tan(tau, e);
            let step = (tau_prime - tau_i_prime) / (1.0 + tau_i_prime * tau_i_prime).sqrt()
                * (1.0 + (1.0 - e2) * tau * tau)
                / ((1.0 - e2) * (1.0 + tau * tau).sqrt());
            tau += step;
            if step.abs() < 1e-14 {
                break;
            }
        }
        let lambda = eta_prime.sinh().atan2(xi_prime.cos());
        (lon0 + lambda.to_degrees(), tau.atan().to_degrees())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct UtmCrs {
    pub zone: usize,
    pub north: bool,
}

impl UtmCrs {
    /// The longitude of the zone's central meridian (degrees).
    fn central_meridian(&self) -> f64 {
        self.zone as f64 * 6.0 - 183.0
    }

    /// What is added to a northing in this zone: 10 000 km south of the
    /// equator.
    fn false_northing(&self) -> f64 {
        if self.north {
            0.0
        } else {
            10_000_000.0
        }
    }

    pub fn optimal_crs(coord: &Coord) -> Self {
        let utm: geomorph::Utm = coord.to_geomorph_coord().into();
        Self {
            zone: utm.zone as usize,
            north: utm.north,
        }
    }

    pub fn to_epsg_str(&self) -> String {
        let mut epsg = "EPSG:32".to_string();

        if self.north {
            epsg += "6";
        } else {
            epsg += "8";
        }
        epsg += &format!("{}", self.zone);
        epsg
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Crs {
    Utm(UtmCrs),
    Proj(String),
}

impl Crs {
    pub fn from_user_input(text: &str) -> Result<Self, String> {
        let utm_result = parse_crs_utm(text);
        if let Ok(utm) = utm_result {
            return Ok(Self::Utm(utm));
        }
        let proj_result = proj_parse_crs(text);

        if let Ok(proj_str) = proj_result {
            if proj_str.contains("+proj=utm")
                & proj_str.contains("+zone=")
                & proj_str.contains("+datum=WGS84")
            {
                let utm_zone: usize = proj_str
                    .split("+zone=")
                    .last()
                    .ok_or("Expected '+zone=' in proj4 string")?
                    .split(" ")
                    .next()
                    .ok_or("Expected whitespace after '+zone=<..>' in proj4 string")?
                    .parse()
                    .map_err(|e| format!("Could not parse '+zone=' value in proj4 string: {e}"))?;
                return Ok(Crs::Utm(UtmCrs {
                    zone: utm_zone,
                    north: !proj_str.contains("+south"),
                }));
            }

            return Ok(Crs::Proj(proj_str.to_string()));
        }

        Err(format!(
            "Could not read CRS.\nInternal error: {}.\nProj error: {}",
            utm_result.err().unwrap_or("None".into()),
            proj_result.err().unwrap_or("None".into())
        ))
    }
}

fn parse_crs_utm(text: &str) -> Result<UtmCrs, String> {
    let parts = text
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect::<Vec<String>>();

    if let Some(first) = parts.first() {
        // Try EPSG:32XXX format
        if first.contains("epsg") {
            let code = first.replace(":", "").replace("epsg", "");
            if !code.starts_with("32") | (code.len() != 5) {
                return Err(format!("EPSG code is not a WGS84 UTM zone: {text}"));
            }

            let (north, start) = match code.chars().nth(2) {
                Some('6') => Ok((true, "326")),
                Some('8') => Ok((false, "328")),
                _ => Err(format!("EPSG code is not a WGS84 UTM zone: {text}")),
            }?;

            if let Ok(zone) = code.replace(start, "").parse::<usize>() {
                return Ok(UtmCrs { zone, north });
            }
        };

        // Try "WGS84 UTM Zone 33 N" format
        if ["wgs84", "wgs1984"].iter().any(|s| first.starts_with(s)) {
            if let Some(mut zone_number) = parts.get(3).map(|v| v.to_string()) {
                let mut north_south: Option<bool> = None;
                if zone_number.contains("n") {
                    north_south = Some(true);
                    zone_number = zone_number.replace("n", "");
                } else if zone_number.contains("s") {
                    north_south = Some(false);
                    zone_number = zone_number.replace("s", "");
                }

                if north_south.is_none() {
                    if let Some(n_s) = parts.get(4) {
                        if n_s.contains("n") {
                            north_south = Some(true);
                        } else if n_s.contains("s") {
                            north_south = Some(false);
                        }
                    }
                }
                if let Ok(number) = zone_number.parse::<usize>() {
                    if let Some(north) = north_south {
                        return Ok(UtmCrs {
                            zone: number,
                            north,
                        });
                    } else {
                        return Err(format!("UTM zone letter not provided or invalid: {text}"));
                    }
                }
            }
        } else {
            return Err(format!("CRS parse error. No 'WGS84' string in {text}"));
        }
    } else {
        return Err(format!("CRS parse error. No whitespaces in {text}"));
    }

    Err(format!("CRS parse error: {text}"))
}

fn proj_parse_crs(text: &str) -> Result<String, String> {
    // An argument, not a shell word, so nothing can be injected; but one
    // starting with `-` would be read as an option of `projinfo`.
    if text.trim_start().starts_with('-') {
        return Err(format!("'{text}' is not a CRS"));
    }
    let child = crate::tools::spawn_tool(
        std::process::Command::new("projinfo")
            .arg(text)
            .stdout(std::process::Stdio::piped()),
    )
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("PROJ (projinfo) cannot be found / is not installed: {e}")
        } else {
            format!("Call error when spawning process: {e}")
        }
    })?;

    let result = child
        .wait_with_output()
        .map_err(|e| format!("Call process error: {e}"))?;
    let parsed = String::from_utf8_lossy(&result.stdout);

    let mut output = String::new();
    // Read output line by line
    let mut next = false;
    for line in parsed.lines() {
        // Check if the line contains a PROJ.4 definition
        if next {
            output.push_str(line.trim());
            break;
        };
        if line.starts_with("PROJ.4 string:") {
            // Extract and print the PROJ.4 string
            next = true;
        }
    }

    match next {
        false => Err("Could not find proj string for given CRS.".into()),
        true => match output.is_empty() {
            true => Err("Proj string was empty.".into()),
            false => Ok(output),
        },
    }
}
fn proj_convert_to_wgs84(x: &[f64], y: &[f64], crs: &str) -> Result<Vec<Coord>, String> {
    proj_convert_crs(x, y, crs, "+init=epsg:4326")
}
fn proj_convert_from_wgs84(x: &[f64], y: &[f64], crs: &str) -> Result<Vec<Coord>, String> {
    proj_convert_crs(x, y, "+init=epsg:4326", crs)
}

fn proj_convert_crs(
    x: &[f64],
    y: &[f64],
    src_crs: &str,
    dst_crs: &str,
) -> Result<Vec<Coord>, String> {
    let mut new_coords = Vec::<Coord>::new();

    let proj_conv_str = format!("{src_crs} +to {dst_crs} -f %.4f")
        .split(" ")
        .map(|s| s.to_string())
        .collect::<Vec<String>>();

    let mut values = Vec::<String>::new();
    for i in 0..x.len() {
        values.push(format!("{} {}", x[i], y[i]));
    }
    let child = crate::tools::spawn_tool(
        std::process::Command::new("cs2cs")
            .args(proj_conv_str)
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped()),
    )
    .map_err(|e| format!("Call error when spawning process: {e}"))?;

    let output = crate::tools::communicate(child, (values.join("\n") + "\n").into_bytes())
        .map_err(|e| format!("Call process error: {e}"))?;
    let parsed = String::from_utf8_lossy(&output.stdout);

    for line in parsed.lines() {
        let values: Vec<f64> = line
            .split_whitespace()
            .filter_map(|s| s.trim().parse::<f64>().ok())
            .collect();

        new_coords.push(Coord {
            x: values[0],
            y: values[1],
        });

        if new_coords.len() == x.len() {
            break;
        };
    }

    Ok(new_coords)
}

pub fn to_wgs84(coords: &[Coord], crs: &Crs) -> Result<Vec<Coord>, String> {
    let mut new_coords = Vec::<Coord>::new();
    match crs {
        Crs::Utm(utm) => {
            for coord in coords {
                new_coords.push(coord.to_wgs84(utm));
            }
        }
        Crs::Proj(proj_str) => {
            let mut eastings = Vec::<f64>::new();
            let mut northings = eastings.clone();

            for coord in coords {
                eastings.push(coord.x);
                northings.push(coord.y);
            }
            new_coords.append(&mut proj_convert_to_wgs84(&eastings, &northings, proj_str)?);
        }
    }

    Ok(new_coords)
}

pub fn from_wgs84(coords: &[Coord], crs: &Crs) -> Result<Vec<Coord>, String> {
    let mut new_coords = Vec::<Coord>::new();
    match crs {
        Crs::Utm(utm) => {
            for coord in coords {
                new_coords.push(coord.conv_from_wgs84(utm));
            }
        }
        Crs::Proj(proj_str) => {
            let mut eastings = Vec::<f64>::new();
            let mut northings = eastings.clone();

            for coord in coords {
                eastings.push(coord.x);
                northings.push(coord.y);
            }
            new_coords.append(&mut proj_convert_from_wgs84(
                &eastings, &northings, proj_str,
            )?);
        }
    }

    Ok(new_coords)
}
#[derive(Clone, Debug)]
pub struct GridMappingSpec {
    pub variable_name: String,
    pub attrs: BTreeMap<String, crate::export::ExportAttr>,
}

fn parse_proj_params(proj_str: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for token in proj_str.split_whitespace() {
        if !token.starts_with('+') {
            continue;
        }
        let token = &token[1..];
        if let Some((k, v)) = token.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        } else {
            // flag-style parameter such as +south
            out.insert(token.to_string(), String::new());
        }
    }
    out
}

fn param_f64(params: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    params.get(key)?.parse::<f64>().ok()
}

fn insert_attr_f64(
    attrs: &mut BTreeMap<String, crate::export::ExportAttr>,
    params: &BTreeMap<String, String>,
    proj_key: &str,
    cf_key: &str,
) {
    if let Some(v) = param_f64(params, proj_key) {
        attrs.insert(cf_key.to_string(), v.into());
    }
}

fn insert_known_ellipsoid_attrs(
    attrs: &mut BTreeMap<String, crate::export::ExportAttr>,
    params: &BTreeMap<String, String>,
) {
    // Prefer explicit numeric axes if present
    if let Some(a) = param_f64(params, "a") {
        attrs.insert("semi_major_axis".into(), a.into());

        if let Some(rf) = param_f64(params, "rf") {
            attrs.insert("inverse_flattening".into(), rf.into());
            return;
        }

        if let Some(b) = param_f64(params, "b") {
            if (a - b).abs() > f64::EPSILON {
                let inv_f = a / (a - b);
                attrs.insert("inverse_flattening".into(), inv_f.into());
            } else {
                attrs.insert("inverse_flattening".into(), 0.0f64.into());
            }
            return;
        }
    }

    // Common named ellipsoids / datums
    if let Some(ellps) = params.get("ellps") {
        match ellps.as_str() {
            "WGS84" => {
                attrs.insert("semi_major_axis".into(), 6378137.0f64.into());
                attrs.insert("inverse_flattening".into(), 298.257223563f64.into());
            }
            "GRS80" => {
                attrs.insert("semi_major_axis".into(), 6378137.0f64.into());
                attrs.insert("inverse_flattening".into(), 298.257222101f64.into());
            }
            _ => {}
        }
        return;
    }

    if Some(&"WGS84".to_string()) == params.get("datum") {
        attrs.insert("semi_major_axis".into(), 6378137.0f64.into());
        attrs.insert("inverse_flattening".into(), 298.257223563f64.into());
    }
}

fn utm_grid_mapping_attrs(utm: &UtmCrs) -> BTreeMap<String, crate::export::ExportAttr> {
    let mut attrs = BTreeMap::new();

    let lon0 = (utm.zone as f64) * 6.0 - 183.0;
    let false_northing = if utm.north { 0.0 } else { 10_000_000.0 };

    attrs.insert("grid_mapping_name".into(), "transverse_mercator".into());
    attrs.insert("scale_factor_at_central_meridian".into(), 0.9996f64.into());
    attrs.insert("longitude_of_central_meridian".into(), lon0.into());
    attrs.insert("latitude_of_projection_origin".into(), 0.0f64.into());
    attrs.insert("false_easting".into(), 500_000.0f64.into());
    attrs.insert("false_northing".into(), false_northing.into());
    attrs.insert("semi_major_axis".into(), 6378137.0f64.into());
    attrs.insert("inverse_flattening".into(), 298.257223563f64.into());

    attrs
}

fn projinfo_to_wkt(definition: &str) -> Result<String, String> {
    let output = crate::tools::spawn_tool(
        std::process::Command::new("projinfo")
            .args(["-o", "WKT2:2019", "--single-line", definition])
            .stdout(std::process::Stdio::piped()),
    )
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("PROJ (projinfo) cannot be found / is not installed: {e}")
        } else {
            format!("Call error when spawning projinfo: {e}")
        }
    })?
    .wait_with_output()
    .map_err(|e| format!("Call process error: {e}"))?;

    let text = String::from_utf8_lossy(&output.stdout);

    let mut take_next_nonempty = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Common case: header line, WKT on the next line
        if take_next_nonempty {
            return Ok(trimmed.to_string());
        }

        // Exact header emitted by projinfo for this output mode
        if trimmed == "WKT2:2019 string:" {
            take_next_nonempty = true;
            continue;
        }

        // Defensive fallback in case some projinfo version puts the WKT on the same line
        if let Some(rest) = trimmed.strip_prefix("WKT2:2019 string:") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Ok(rest.to_string());
            }
            take_next_nonempty = true;
        }
    }

    Err("Could not extract WKT2:2019 from projinfo output.".into())
}

fn proj_grid_mapping_attrs(
    crs_str: &str,
) -> Result<Option<BTreeMap<String, crate::export::ExportAttr>>, String> {
    let proj_str = proj_parse_crs(crs_str)?;
    let params = parse_proj_params(&proj_str);

    // If the PROJ string is really just WGS84 UTM, normalize to the UTM branch
    if params.get("proj").map(|s| s.as_str()) == Some("utm") {
        if let Some(zone_s) = params.get("zone") {
            if let Ok(zone) = zone_s.parse::<usize>() {
                let utm = UtmCrs {
                    zone,
                    north: !params.contains_key("south"),
                };
                let mut attrs = utm_grid_mapping_attrs(&utm);
                if let Ok(wkt) = projinfo_to_wkt(crs_str) {
                    attrs.insert("crs_wkt".into(), wkt.into());
                }
                return Ok(Some(attrs));
            }
        }
    }

    let proj_name = match params.get("proj").map(|s| s.as_str()) {
        Some("tmerc") => "transverse_mercator",
        Some("merc") => "mercator",
        Some("laea") => "lambert_azimuthal_equal_area",
        Some("aeqd") => "azimuthal_equidistant",
        Some("ortho") => "orthographic",
        Some("stere") => "stereographic",
        Some("geos") => "vertical_perspective",
        _ => return Ok(None),
    };

    let mut attrs = BTreeMap::new();
    attrs.insert("grid_mapping_name".into(), proj_name.into());

    match proj_name {
        "transverse_mercator" => {
            insert_attr_f64(
                &mut attrs,
                &params,
                "k_0",
                "scale_factor_at_central_meridian",
            );
            if !attrs.contains_key("scale_factor_at_central_meridian") {
                insert_attr_f64(&mut attrs, &params, "k", "scale_factor_at_central_meridian");
            }
            insert_attr_f64(
                &mut attrs,
                &params,
                "lon_0",
                "longitude_of_central_meridian",
            );
            insert_attr_f64(
                &mut attrs,
                &params,
                "lat_0",
                "latitude_of_projection_origin",
            );
            insert_attr_f64(&mut attrs, &params, "x_0", "false_easting");
            insert_attr_f64(&mut attrs, &params, "y_0", "false_northing");
        }
        "mercator" => {
            insert_attr_f64(
                &mut attrs,
                &params,
                "lon_0",
                "longitude_of_projection_origin",
            );
            insert_attr_f64(&mut attrs, &params, "lat_ts", "standard_parallel");
            if !attrs.contains_key("standard_parallel") {
                insert_attr_f64(
                    &mut attrs,
                    &params,
                    "k_0",
                    "scale_factor_at_projection_origin",
                );
                if !attrs.contains_key("scale_factor_at_projection_origin") {
                    insert_attr_f64(
                        &mut attrs,
                        &params,
                        "k",
                        "scale_factor_at_projection_origin",
                    );
                }
            }
            insert_attr_f64(&mut attrs, &params, "x_0", "false_easting");
            insert_attr_f64(&mut attrs, &params, "y_0", "false_northing");
        }
        "lambert_azimuthal_equal_area" | "azimuthal_equidistant" | "orthographic" => {
            insert_attr_f64(
                &mut attrs,
                &params,
                "lon_0",
                "longitude_of_projection_origin",
            );
            insert_attr_f64(
                &mut attrs,
                &params,
                "lat_0",
                "latitude_of_projection_origin",
            );
            insert_attr_f64(&mut attrs, &params, "x_0", "false_easting");
            insert_attr_f64(&mut attrs, &params, "y_0", "false_northing");
        }
        "stereographic" => {
            insert_attr_f64(
                &mut attrs,
                &params,
                "lon_0",
                "longitude_of_projection_origin",
            );
            insert_attr_f64(
                &mut attrs,
                &params,
                "lat_0",
                "latitude_of_projection_origin",
            );
            insert_attr_f64(
                &mut attrs,
                &params,
                "k_0",
                "scale_factor_at_projection_origin",
            );
            if !attrs.contains_key("scale_factor_at_projection_origin") {
                insert_attr_f64(
                    &mut attrs,
                    &params,
                    "k",
                    "scale_factor_at_projection_origin",
                );
            }
            insert_attr_f64(&mut attrs, &params, "x_0", "false_easting");
            insert_attr_f64(&mut attrs, &params, "y_0", "false_northing");
        }
        "vertical_perspective" => {
            insert_attr_f64(
                &mut attrs,
                &params,
                "lon_0",
                "longitude_of_projection_origin",
            );
            insert_attr_f64(
                &mut attrs,
                &params,
                "lat_0",
                "latitude_of_projection_origin",
            );
            insert_attr_f64(&mut attrs, &params, "h", "perspective_point_height");
            insert_attr_f64(&mut attrs, &params, "x_0", "false_easting");
            insert_attr_f64(&mut attrs, &params, "y_0", "false_northing");
        }
        _ => {}
    }

    insert_known_ellipsoid_attrs(&mut attrs, &params);

    if let Ok(wkt) = projinfo_to_wkt(crs_str) {
        attrs.insert("crs_wkt".into(), wkt.into());
    }

    Ok(Some(attrs))
}

pub fn build_grid_mapping_from_crs(crs_str: &str) -> Result<Option<GridMappingSpec>, String> {
    let crs = Crs::from_user_input(crs_str)?;
    let attrs = match crs {
        Crs::Utm(utm) => {
            let mut attrs = utm_grid_mapping_attrs(&utm);

            // Optional enhancement: if PROJ is available, also attach crs_wkt using the EPSG code
            if let Ok(wkt) = projinfo_to_wkt(crs_str) {
                attrs.insert("crs_wkt".into(), wkt.into());
            }

            Some(attrs)
        }
        Crs::Proj(_) => proj_grid_mapping_attrs(crs_str)?,
    };

    Ok(attrs.map(|attrs| GridMappingSpec {
        variable_name: "projected_crs".into(),
        attrs,
    }))
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use super::{Coord, Crs, UtmCrs};

    fn coords_approx_eq(first: &Coord, second: &Coord, precision: f64) -> bool {
        let xdiff = first.x - second.x;
        let ydiff = first.y - second.y;
        (xdiff.powi(2) + ydiff.powi(2)).sqrt() < precision
    }

    fn make_test_cases() -> Vec<(String, Crs)> {
        vec![
            (
                "EPSG:32633".into(),
                Crs::Utm(UtmCrs {
                    zone: 33,
                    north: true,
                }),
            ),
            (
                "WGS84 UTM Zone 33S".into(),
                Crs::Utm(UtmCrs {
                    zone: 33,
                    north: false,
                }),
            ),
            (
                "EPSG:3006".into(),
                Crs::Proj("+proj=utm +zone=33 +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +units=m +no_defs +type=crs".into()),

            )
        ]
    }

    #[test]
    fn test_crs_from_user() {
        for (crs_str, expected) in make_test_cases() {
            let _parsed_proj = super::proj_parse_crs(&crs_str).unwrap();

            let parsed = super::Crs::from_user_input(&crs_str).unwrap();

            println!("Expected: {:?}", expected);
            println!("Received: {:?}", parsed);

            assert_eq!(parsed.type_id(), expected.type_id());
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_crs_noproj() {
        // This test simulates machines without PROJ installed. UTM CRSes should work but not others.
        crate::tools::without_tools(|| {
            // This "complex" CRS should fail
            let res = super::Crs::from_user_input("EPSG:3006");
            let msg = res.unwrap_err();
            assert!(
                msg.contains("PROJ (projinfo) cannot be found / is not installed"),
                "{msg}"
            );
            // A UTM CRS should still work.
            let parsed = super::Crs::from_user_input("EPSG:32633").unwrap();

            if let Crs::Utm(crs) = &parsed {
                assert_eq!(crs.zone, 33);
                assert_eq!(crs.north, true);
            } else {
                panic!("Wrong type of parsed CRS: {parsed:?}");
            }
            let coords = vec![
                Coord { x: 15., y: 78. },
                Coord { x: 0., y: 1. },
                Coord { x: 15., y: -78. },
            ];
            let conv = super::from_wgs84(&coords, &parsed).unwrap();
            let conv_back = super::to_wgs84(&conv, &parsed).unwrap();

            for i in 0..conv_back.len() {
                println!("{:?} -> {:?} -> {:?}", coords[i], conv[i], conv_back[i]);

                assert!(coords_approx_eq(&coords[i], &conv_back[i], 0.01));
            }
        });
    }

    #[test]
    fn test_parse_utm() {
        for (crs_str, expected_any) in make_test_cases() {
            if let Crs::Utm(expected) = expected_any {
                let res = super::parse_crs_utm(&crs_str);

                assert_eq!(res, Ok(expected));
            }
        }
        let failures = vec![
            ("EPSG:3006", "EPSG code is not a WGS84"),
            ("ETRS89 UTM Zone 33N", "No 'WGS84' string in"),
            ("WGS84UTMZone33N", "CRS parse error"),
            (
                "WGS84 UTM Zone 33 X",
                "UTM zone letter not provided or invalid",
            ),
        ];

        for (failure, expected_err) in failures {
            if let Err(err_str) = super::parse_crs_utm(&failure) {
                if !err_str.contains(expected_err) {
                    panic!("Expected '{expected_err}' in '{err_str}'");
                }
            } else {
                panic!("Should have failed on {failure}")
            }
        }
    }

    #[test]
    fn test_optimal_crs() {
        let crs = Crs::Utm(UtmCrs::optimal_crs(&Coord { y: 78., x: 15. }));

        match crs {
            Crs::Utm(utm) => {
                assert_eq!(utm.zone, 33);
                assert_eq!(utm.north, true);
            }
            _ => panic!(),
        };
    }

    #[test]
    fn utm_agrees_with_proj_both_ways_to_a_millimetre() {
        // #480. From `cs2cs EPSG:4326 EPSG:326xx/327xx` (PROJ 9).
        for (lon, lat, crs, easting, northing) in [
            (17.2, 77.81, "EPSG:32633", 551846.0855, 8638138.6911),
            (13.0, 78.9, "EPSG:32633", 457023.5639, 8759549.3573),
            (
                18.42406,
                -33.92487,
                "WGS84 UTM Zone 34S",
                261877.8164,
                6243185.5892,
            ),
        ] {
            let crs = Crs::from_user_input(crs).unwrap();
            let projected = super::from_wgs84(&[Coord { x: lon, y: lat }], &crs).unwrap()[0];
            assert!(
                (projected.x - easting).abs() < 0.001 && (projected.y - northing).abs() < 0.001,
                "{lon} {lat} -> {projected:?}"
            );
            let back = super::to_wgs84(
                &[Coord {
                    x: easting,
                    y: northing,
                }],
                &crs,
            )
            .unwrap()[0];
            // 1e-8 degrees is about a millimetre.
            assert!(
                (back.x - lon).abs() < 1e-8 && (back.y - lat).abs() < 1e-8,
                "{easting} {northing} -> {back:?}"
            );
        }
    }

    #[test]
    fn test_crs_convert() {
        let coords = vec![
            Coord { x: 15., y: 78. },
            Coord { x: 0., y: 1. },
            Coord { x: 15., y: -78. },
        ];
        for (crs_str, _) in make_test_cases() {
            println!("Converting with {crs_str}");
            let parsed = super::Crs::from_user_input(&crs_str).unwrap();
            let conv = super::from_wgs84(&coords, &parsed).unwrap();
            let conv_back = super::to_wgs84(&conv, &parsed).unwrap();

            for i in 0..conv_back.len() {
                println!("{:?} -> {:?} -> {:?}", coords[i], conv[i], conv_back[i]);

                assert!(coords_approx_eq(&coords[i], &conv_back[i], 0.01));
            }
        }
    }
    #[test]
    // macOS was excluded after a random failure (#108): another test unset
    // `PATH` process-wide while this one ran. The tests that simulate a
    // missing PROJ now use `tools::without_tools`, which touches only their
    // own thread.
    #[cfg(not(target_os = "windows"))] // Added windows 2026-04-6
    fn test_projinfo_to_wkt() {
        let retval = super::projinfo_to_wkt("EPSG:32633").unwrap();

        println!("{}", retval);
        assert!(retval.starts_with("PROJCRS[\"WGS 84"));
    }
}
