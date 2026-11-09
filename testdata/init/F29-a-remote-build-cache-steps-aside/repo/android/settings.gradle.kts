rootProject.name = "shop"

include(":app")

buildCache {
    remote<HttpBuildCache> {
        url = uri("https://cache.example.com/cache/")
        isPush = System.getenv("CI") != null
    }
}
