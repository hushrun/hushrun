# F13: .NET's coverage collector, then ReportGenerator

**Pattern.** `dotnet test --collect:"XPlat Code Coverage"`, then
`danielpalme/ReportGenerator-GitHub-Action` reading `**/coverage.cobertura.xml`.

**Mirrors.** .NET projects that collect coverage with coverlet's collector.

**Expected after Phase 1.** init reads the collector as writing a report (step 1.1), wraps the line,
and predicts `ci: runs` with `ciWhy: reports`, naming the ReportGenerator action.
