using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using Microsoft.Extensions.Options;
using Octo.Models.Settings;

namespace Octo.Services.ListenBrainz;

/// <summary>
/// Submits listens to ListenBrainz for plays of external tracks. One call per
/// completed play, best effort: a failure is logged and never reaches the client,
/// because a listen is a record of something that already happened.
/// </summary>
public sealed class ListenBrainzService
{
    public const string ClientName = "listenbrainz";
    internal const string SubmitUrl = "https://api.listenbrainz.org/1/submit-listens";
    internal const string ValidateUrl = "https://api.listenbrainz.org/1/validate-token";

    private readonly IHttpClientFactory _httpClientFactory;
    private readonly IOptionsMonitor<ListenBrainzSettings> _settings;
    private readonly string _version;
    private readonly ILogger<ListenBrainzService> _logger;

    public ListenBrainzService(IHttpClientFactory httpClientFactory,
        IOptionsMonitor<ListenBrainzSettings> settings, ILogger<ListenBrainzService> logger)
    {
        _httpClientFactory = httpClientFactory;
        _settings = settings;
        _logger = logger;
        _version = typeof(ListenBrainzService).Assembly
            .GetCustomAttributes(typeof(System.Reflection.AssemblyInformationalVersionAttribute), false)
            .OfType<System.Reflection.AssemblyInformationalVersionAttribute>()
            .FirstOrDefault()?.InformationalVersion.Split('+')[0] ?? "dev";
    }

    /// <summary>True when this listener has a token and submission is switched on.</summary>
    public bool IsEnabledFor(string username) => _settings.CurrentValue.TokenFor(username) is not null;

    /// <summary>
    /// Records one completed play. Returns true when ListenBrainz accepted it, false
    /// when nothing was sent (no token) or the submission failed.
    /// </summary>
    public async Task<bool> SubmitListenAsync(string username, string artist, string title,
        string? album, int? durationSeconds, DateTime listenedAtUtc,
        CancellationToken cancellationToken = default)
    {
        var token = _settings.CurrentValue.TokenFor(username);
        if (token is null || string.IsNullOrWhiteSpace(artist) || string.IsNullOrWhiteSpace(title))
            return false;

        var additional = new Dictionary<string, object>
        {
            ["media_player"] = "Octo",
            ["submission_client"] = "Octo",
            ["submission_client_version"] = _version,
        };
        if (durationSeconds is > 0) additional["duration_ms"] = durationSeconds.Value * 1000;
        var metadata = new Dictionary<string, object>
        {
            ["artist_name"] = artist.Trim(),
            ["track_name"] = title.Trim(),
            ["additional_info"] = additional,
        };
        if (!string.IsNullOrWhiteSpace(album)) metadata["release_name"] = album.Trim();
        var body = JsonSerializer.Serialize(new
        {
            listen_type = "single",
            payload = new[]
            {
                new
                {
                    listened_at = new DateTimeOffset(DateTime.SpecifyKind(listenedAtUtc, DateTimeKind.Utc))
                        .ToUnixTimeSeconds(),
                    track_metadata = metadata,
                }
            }
        });

        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, SubmitUrl)
            {
                Content = new StringContent(body, Encoding.UTF8, "application/json"),
            };
            request.Headers.Authorization = new AuthenticationHeaderValue("Token", token);
            using var response = await _httpClientFactory.CreateClient(ClientName)
                .SendAsync(request, cancellationToken);
            if (response.IsSuccessStatusCode)
            {
                _logger.LogInformation("ListenBrainz listen submitted for {User}: {Artist} - {Title}",
                    username, artist, title);
                return true;
            }
            _logger.LogWarning("ListenBrainz rejected a listen for {User} with HTTP {Status}: {Body}",
                username, (int)response.StatusCode,
                (await response.Content.ReadAsStringAsync(cancellationToken)).Trim());
            return false;
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            _logger.LogWarning(ex, "ListenBrainz submission failed for {User}", username);
            return false;
        }
    }

    /// <summary>Asks ListenBrainz whether a token is valid and whose it is.</summary>
    public async Task<(bool Valid, string? UserName, string Detail)> ValidateTokenAsync(string token,
        CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(token)) return (false, null, "No token configured.");
        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, ValidateUrl);
            request.Headers.Authorization = new AuthenticationHeaderValue("Token", token.Trim());
            using var response = await _httpClientFactory.CreateClient(ClientName)
                .SendAsync(request, cancellationToken);
            var json = await response.Content.ReadAsStringAsync(cancellationToken);
            if (!response.IsSuccessStatusCode) return (false, null, $"HTTP {(int)response.StatusCode}");
            using var document = JsonDocument.Parse(json);
            var root = document.RootElement;
            var valid = root.TryGetProperty("valid", out var validElement) && validElement.GetBoolean();
            var user = root.TryGetProperty("user_name", out var userElement) ? userElement.GetString() : null;
            return (valid, user, valid ? $"Valid, belongs to {user}." : "ListenBrainz says this token is not valid.");
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            return (false, null, ex.Message);
        }
    }
}
