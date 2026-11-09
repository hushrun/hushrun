//! What to grant when a copy is denied (A-3): for known registries, the fallback warning names
//! the missing permission.

use crate::registry::Repository;

/// The permissions a copy from `source` to `destination` needs, for registries hushrun knows.
pub fn permissions(source: &Repository, destination: &Repository) -> Option<String> {
    let artifact_registry = |r: &Repository| r.host.ends_with("-docker.pkg.dev");
    let ecr = |r: &Repository| r.host.contains(".dkr.ecr.") && r.host.ends_with(".amazonaws.com");
    let acr = |r: &Repository| r.host.ends_with(".azurecr.io");
    let ghcr = |r: &Repository| r.host == "ghcr.io";
    if artifact_registry(source) || artifact_registry(destination) {
        Some(format!(
            "grant roles/artifactregistry.reader on {source} and roles/artifactregistry.writer on {destination}"
        ))
    } else if ecr(source) || ecr(destination) {
        Some(format!(
            "allow ecr:BatchGetImage and ecr:GetDownloadUrlForLayer on {source}, and ecr:BatchCheckLayerAvailability, ecr:InitiateLayerUpload, ecr:UploadLayerPart, ecr:CompleteLayerUpload and ecr:PutImage on {destination}"
        ))
    } else if acr(source) || acr(destination) {
        Some(format!(
            "assign AcrPull on {} and AcrPush on {}",
            source.host, destination.host
        ))
    } else if ghcr(source) || ghcr(destination) {
        Some(
            "use a token with read:packages for the source and write:packages for the destination"
                .to_string(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: A-3
    #[test]
    fn artifact_registry_names_both_roles() {
        let source = Repository::parse("us-central1-docker.pkg.dev/acme-test/api/go").unwrap();
        let destination = Repository::parse("us-central1-docker.pkg.dev/acme-prod/api/go").unwrap();
        let hint = permissions(&source, &destination).unwrap();
        assert!(hint.contains(
            "roles/artifactregistry.reader on us-central1-docker.pkg.dev/acme-test/api/go"
        ));
        assert!(hint.contains(
            "roles/artifactregistry.writer on us-central1-docker.pkg.dev/acme-prod/api/go"
        ));
        let ecr = Repository::parse("1.dkr.ecr.us-east-1.amazonaws.com/app").unwrap();
        assert!(permissions(&ecr, &ecr).unwrap().contains("ecr:PutImage"));
        let other = Repository::parse("registry.example.com/app").unwrap();
        assert_eq!(permissions(&other, &other), None);
    }
}
