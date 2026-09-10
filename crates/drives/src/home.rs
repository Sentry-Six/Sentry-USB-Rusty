//! Shared Home classification for local charging and encrypted Cloud metadata.
use anyhow::{Result, ensure};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HomeGeofence {
    pub latitude: f64,
    pub longitude: f64,
    pub radius_m: f64,
}
impl HomeGeofence {
    pub fn new(latitude:f64,longitude:f64,radius_m:f64)->Result<Self> {
        ensure!(latitude.is_finite() && (-90.0..=90.0).contains(&latitude)
            && longitude.is_finite() && radius_m.is_finite() && radius_m>0.0 && radius_m<=100_000.0,
            "invalid Home geofence");
        Ok(Self {latitude,longitude:(longitude+180.0).rem_euclid(360.0)-180.0,radius_m})
    }
    pub fn contains(&self,latitude:Option<f64>,longitude:Option<f64>)->bool {
        let (Some(latitude),Some(longitude))=(latitude,longitude) else {return false};
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) || !longitude.is_finite() {return false}
        let longitude=(longitude+180.0).rem_euclid(360.0)-180.0;
        let delta_lat=(latitude-self.latitude).to_radians();let delta_lon=(longitude-self.longitude).to_radians();
        let haversine=(delta_lat/2.0).sin().powi(2)+self.latitude.to_radians().cos()*latitude.to_radians().cos()*(delta_lon/2.0).sin().powi(2);
        2.0*6_371_000.0*haversine.sqrt().min(1.0).asin()<=self.radius_m
    }
    pub fn tuple(self)->(f64,f64,f64) {(self.latitude,self.longitude,self.radius_m)}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_center_equator_and_world_copies_are_valid_locations() {
        let home=HomeGeofence::new(0.0,0.0,120.0).unwrap();
        assert!(home.contains(Some(0.0),Some(0.0)));
        assert!(home.contains(Some(0.0),Some(360.0)));
        assert!(home.contains(Some(0.0005),Some(0.0005)));
        assert!(!home.contains(Some(0.01),Some(0.0)));
        assert_eq!(HomeGeofence::new(0.0,360.0,120.0).unwrap(),home);
    }
    #[test]
    fn malformed_settings_and_missing_or_invalid_fixes_cannot_classify_home() {
        for (lat,lon,radius) in [(91.0,0.0,120.0),(0.0,f64::NAN,120.0),(0.0,0.0,0.0),(0.0,0.0,f64::INFINITY),(0.0,0.0,100_001.0)] {
            assert!(HomeGeofence::new(lat,lon,radius).is_err());
        }
        let home=HomeGeofence::new(0.0,0.0,120.0).unwrap();
        for (lat,lon) in [(None,Some(0.0)),(Some(0.0),None),(Some(f64::NAN),Some(0.0)),(Some(91.0),Some(0.0)),(Some(0.0),Some(f64::INFINITY))] {
            assert!(!home.contains(lat,lon));
        }
    }
    #[test]
    fn dateline_neighbors_and_radius_changes_classify_consistently() {
        assert!(HomeGeofence::new(0.0,179.9998,120.0).unwrap().contains(Some(0.0),Some(-179.9998)));
        assert!(!HomeGeofence::new(0.0,179.9998,20.0).unwrap().contains(Some(0.0),Some(-179.9998)));
    }
}
