using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.Logging;
using Moq;
using Octo.Services.YouTube;

namespace Octo.Tests;

/// <summary>
/// The admin UI saves a cleared shim field as an empty string, which is not null, so
/// a null-only fallback left every shim request relative and the factory client with
/// no base address. The resolver has to treat absent and blank the same way.
/// </summary>
public class YouTubeResolverBaseUrlTests
{
    private static YouTubeResolver Build(string? shimUrl)
    {
        var values = new Dictionary<string, string?>();
        if (shimUrl is not null) values["YouTube:ShimUrl"] = shimUrl;
        var config = new ConfigurationBuilder().AddInMemoryCollection(values).Build();

        return new YouTubeResolver(
            new Mock<IHttpClientFactory>().Object,
            config,
            new Mock<ILogger<YouTubeResolver>>().Object);
    }

    [Fact]
    public void AbsentSettingUsesTheComposeServiceName()
    {
        Assert.Equal(YouTubeResolver.DefaultBaseUrl, Build(null).BaseUrl);
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    public void BlankSettingUsesTheComposeServiceName(string shimUrl)
    {
        Assert.Equal(YouTubeResolver.DefaultBaseUrl, Build(shimUrl).BaseUrl);
    }

    [Theory]
    [InlineData("http://shim.local:8080", "http://shim.local:8080")]
    [InlineData("http://shim.local:8080/", "http://shim.local:8080")]
    [InlineData("  http://shim.local:8080/  ", "http://shim.local:8080")]
    public void ConfiguredSettingIsKeptWithoutATrailingSlash(string shimUrl, string expected)
    {
        Assert.Equal(expected, Build(shimUrl).BaseUrl);
    }
}
